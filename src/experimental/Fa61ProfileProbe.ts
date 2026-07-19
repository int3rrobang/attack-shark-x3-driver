import * as HID from 'node-hid';
import { HIDAsync } from 'node-hid';
import { ParamsError } from '../errors.js';
import { delay } from '../utils/delay.js';

const VID = 0x1d57;
const FA61_PID = 0xfa61;
const COL04_PATTERN = /col04/i;

export const FA61_REPORT_LENGTH = Object.freeze({
	DPI: 0x38,
	PREFERENCES: 0x0f,
	BUTTONS: 0x3b,
	BULK_PROFILE: 0x80,
	VERSION: 0x08,
	PROFILE_METADATA: 0x0a,
});

const TARGETED_PROFILE_REPORTS: Readonly<Record<number, true>> = Object.freeze({
	0x04: true,
	0x05: true,
	0x08: true,
});

export interface Fa61DeviceIdentity {
	readonly path: string;
	readonly product: string | undefined;
	readonly serialNumber: string | undefined;
	readonly vendorId: number;
	readonly productId: number;
	readonly interface: number;
}

export interface Fa61Snapshot {
	readonly capturedAt: string;
	readonly device: Fa61DeviceIdentity;
	readonly version: string;
	readonly profileMetadata: string;
	readonly dpi: string;
	readonly preferences: string;
	readonly buttons: string;
}

export interface ProfileMetadata {
	readonly current: number;
	readonly maximum: number;
}

export interface ProfileObservation {
	readonly metadata: ProfileMetadata;
	readonly profileMetadataHex: string;
	readonly dpiHex: string;
}

export function hasLoadedDistinctProfile(
	observation: ProfileObservation,
	targetProfile: number,
	referenceDpiHex: string,
): boolean {
	if (observation.metadata.current !== targetProfile) return false;

	const observed = Buffer.from(observation.dpiHex, 'hex');
	const reference = Buffer.from(referenceDpiHex, 'hex');
	assertReport(observed, 0x04, FA61_REPORT_LENGTH.DPI, 'observed DPI');
	assertReport(reference, 0x04, FA61_REPORT_LENGTH.DPI, 'reference DPI');
	return !observed.subarray(3).equals(reference.subarray(3));
}

function assertByte(value: number, name: string): void {
	if (!Number.isInteger(value) || value < 0 || value > 0xff) {
		throw new ParamsError(name, `${name} must be an integer from 0 to 255, got ${value}`);
	}
}

function assertProfile(profile: number, name: string): void {
	if (!Number.isInteger(profile) || profile < 1 || profile > 5) {
		throw new ParamsError(name, `${name} must be a one-based profile from 1 to 5, got ${profile}`);
	}
}

function assertReport(report: Buffer, reportId: number, length: number, name: string): void {
	if (report.length !== length || report[0] !== reportId) {
		throw new ParamsError(
			name,
			`${name} must be a ${length}-byte report starting with 0x${reportId.toString(16).padStart(2, '0')}`,
		);
	}
}

export function buildReadSelector(reportId: number, reportLength: number, targetProfile: number = 1): Buffer {
	assertByte(reportId, 'reportId');
	assertByte(reportLength, 'reportLength');
	assertProfile(targetProfile, 'targetProfile');
	return Buffer.from([0xa0, reportId, reportLength, 0x00, targetProfile, 0x00, 0x00, 0x00]);
}

export function buildProfileSelector(current: number, maximum: number = 5): Buffer {
	if (!Number.isInteger(current) || !Number.isInteger(maximum) || current < 1 || current > maximum || maximum > 5) {
		throw new ParamsError(
			'profile',
			`Profiles must satisfy 1 <= current <= maximum <= 5, got ${current}/${maximum}`,
		);
	}

	return Buffer.from([0x0c, 0x0a, current, ~current & 0xff, maximum, ~maximum & 0xff]);
}

export function decodeProfileMetadata(report: Buffer): ProfileMetadata {
	assertReport(report, 0x0c, FA61_REPORT_LENGTH.PROFILE_METADATA, 'profile metadata');
	if (report[2] !== 0x01) {
		throw new ParamsError(
			'profileMetadata',
			`Expected profile metadata subtype 0x01, got 0x${report[2]?.toString(16)}`,
		);
	}

	const current = report.readUInt8(3);
	const currentComplement = report.readUInt8(4);
	const maximum = report.readUInt8(5);
	const maximumComplement = report.readUInt8(6);
	if (((current + currentComplement) & 0xff) !== 0xff || ((maximum + maximumComplement) & 0xff) !== 0xff) {
		throw new ParamsError('profileMetadata', 'Profile metadata contains an invalid complement pair');
	}
	if (current < 1 || current > maximum || maximum > 5) {
		throw new ParamsError(
			'profileMetadata',
			`Profile metadata is outside 1 <= current <= maximum <= 5: ${current}/${maximum}`,
		);
	}

	return { current, maximum };
}

export function decodeFirstDpi(report: Buffer): number {
	assertReport(report, 0x04, FA61_REPORT_LENGTH.DPI, 'DPI');
	const raw = (report.readUInt8(16) << 8) | report.readUInt8(8);
	return (raw + 1) * 50;
}

export function buildSingleStageDpi(source: Buffer, dpi: number, targetProfile: number): Buffer {
	assertReport(source, 0x04, FA61_REPORT_LENGTH.DPI, 'DPI');
	assertProfile(targetProfile, 'targetProfile');
	if (!Number.isInteger(dpi) || dpi < 50 || dpi > 26000 || dpi % 50 !== 0) {
		throw new ParamsError('dpi', `DPI must be a multiple of 50 from 50 to 26000, got ${dpi}`);
	}

	const packet = Buffer.from(source);
	const raw = dpi / 50 - 1;
	packet[2] = targetProfile;
	packet[5] = 0x01;
	packet.fill(0x00, 8, 24);
	packet[8] = raw & 0xff;
	packet[16] = (raw >> 8) & 0xff;
	packet[24] = 0x01;
	writeSectionChecksum(packet, 3, 49, 50);
	return packet;
}

export function buildButtonAction(source: Buffer, offset: number, action: number, targetProfile: number): Buffer {
	assertReport(source, 0x08, FA61_REPORT_LENGTH.BUTTONS, 'button');
	assertByte(action, 'action');
	assertProfile(targetProfile, 'targetProfile');
	if (!Number.isInteger(offset) || offset < 3 || offset > 54 || (offset - 3) % 3 !== 0) {
		throw new ParamsError('offset', `Button offset must identify a three-byte slot from 3 to 54, got ${offset}`);
	}

	const packet = Buffer.from(source);
	packet[2] = targetProfile;
	packet[offset] = action;
	packet[offset + 1] = 0x00;
	packet[offset + 2] = 0x00;
	writeSectionChecksum(packet, 3, 56, 57);
	return packet;
}

export function calculateSectionChecksum(packet: Buffer, start: number, end: number): number {
	let checksum = 0;
	for (let index = start; index <= end; index++) checksum = (checksum + packet.readUInt8(index)) & 0xffff;
	return checksum;
}

function writeSectionChecksum(packet: Buffer, start: number, end: number, checksumOffset: number): void {
	packet.writeUInt16BE(calculateSectionChecksum(packet, start, end), checksumOffset);
}

export class Fa61ProfileProbe {
	private constructor(
		public readonly device: Fa61DeviceIdentity,
		public readonly delayMs: number,
	) {}

	static async discover(options: { delayMs?: number; pathContains?: string } = {}): Promise<Fa61ProfileProbe> {
		const delayMs = options.delayMs ?? 500;
		if (!Number.isInteger(delayMs) || delayMs < 100) {
			throw new ParamsError('delayMs', `Probe delay must be an integer of at least 100 ms, got ${delayMs}`);
		}

		const pathFilter = options.pathContains?.toLowerCase();
		const candidates = (await HID.devicesAsync()).filter(
			(device) =>
				device.vendorId === VID &&
				device.productId === FA61_PID &&
				COL04_PATTERN.test(device.path ?? '') &&
				(pathFilter === undefined || (device.path ?? '').toLowerCase().includes(pathFilter)),
		);
		if (candidates.length !== 1) {
			throw new Error(
				candidates.length === 0
					? 'No FA61 interface-2 Col04 device found'
					: `Found ${candidates.length} FA61 Col04 devices; use --path-contains to select one`,
			);
		}

		const candidate = candidates[0];
		if (!candidate) throw new Error('FA61 Col04 device disappeared during discovery');
		if (!candidate.path) throw new Error('FA61 Col04 device has no HID path');
		return new Fa61ProfileProbe(
			{
				path: candidate.path,
				product: candidate.product,
				serialNumber: candidate.serialNumber,
				vendorId: candidate.vendorId,
				productId: candidate.productId,
				interface: candidate.interface,
			},
			delayMs,
		);
	}

	async readReport(reportId: number, reportLength: number, targetProfile?: number): Promise<Buffer> {
		if (TARGETED_PROFILE_REPORTS[reportId] && targetProfile === undefined) {
			throw new ParamsError(
				'targetProfile',
				`Report 0x${reportId.toString(16).padStart(2, '0')} requires an explicit one-based target profile`,
			);
		}
		const selectorProfile = targetProfile ?? 1;
		assertProfile(selectorProfile, 'targetProfile');

		const device = await HIDAsync.open(this.device.path);
		try {
			const selector = buildReadSelector(reportId, reportLength, selectorProfile);
			const sent = await device.sendFeatureReport(selector);
			if (sent !== selector.length) throw new Error(`Read selector wrote ${sent}/${selector.length} bytes`);
			await delay(this.delayMs);

			const status = Buffer.from(await device.getFeatureReport(0xa0, 0x08));
			if (status.length !== 8 || status[0] !== 0xa0 || status[1] !== 0x01) {
				throw new Error(`Read selector failed: ${status.toString('hex')}`);
			}
			return Buffer.from(await device.getFeatureReport(reportId, reportLength));
		} finally {
			await device.close();
		}
	}

	async writeReport(packet: Buffer): Promise<void> {
		const device = await HIDAsync.open(this.device.path);
		try {
			const stablePacket = Buffer.from(packet);
			const sent = await device.sendFeatureReport(stablePacket);
			if (sent !== stablePacket.length)
				throw new Error(`Feature report wrote ${sent}/${stablePacket.length} bytes`);
			await delay(this.delayMs);
		} finally {
			await device.close();
		}
	}

	async selectProfile(current: number, maximum: number = 5): Promise<void> {
		await this.writeReport(buildProfileSelector(current, maximum));
	}

	async observeProfile(targetProfile: number): Promise<ProfileObservation> {
		assertProfile(targetProfile, 'targetProfile');
		const profileMetadata = await this.readReport(0x0c, FA61_REPORT_LENGTH.PROFILE_METADATA);
		const dpi = await this.readReport(0x04, FA61_REPORT_LENGTH.DPI, targetProfile);
		return {
			metadata: decodeProfileMetadata(profileMetadata),
			profileMetadataHex: profileMetadata.toString('hex'),
			dpiHex: dpi.toString('hex'),
		};
	}

	async snapshot(): Promise<Fa61Snapshot> {
		const profileMetadata = await this.readReport(0x0c, FA61_REPORT_LENGTH.PROFILE_METADATA);
		const targetProfile = decodeProfileMetadata(profileMetadata).current;
		const version = await this.readReport(0x0b, FA61_REPORT_LENGTH.VERSION);
		const dpi = await this.readReport(0x04, FA61_REPORT_LENGTH.DPI, targetProfile);
		const preferences = await this.readReport(0x05, FA61_REPORT_LENGTH.PREFERENCES, targetProfile);
		const buttons = await this.readReport(0x08, FA61_REPORT_LENGTH.BUTTONS, targetProfile);
		return {
			capturedAt: new Date().toISOString(),
			device: this.device,
			version: version.toString('hex'),
			profileMetadata: profileMetadata.toString('hex'),
			dpi: dpi.toString('hex'),
			preferences: preferences.toString('hex'),
			buttons: buttons.toString('hex'),
		};
	}
}
