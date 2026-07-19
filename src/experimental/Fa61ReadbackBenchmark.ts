import { ParamsError } from '../errors.js';

export const READBACK_REPORTS = Object.freeze({
	version: Object.freeze({ id: 0x0b, length: 0x08, targeted: false }),
	profileMetadata: Object.freeze({ id: 0x0c, length: 0x0a, targeted: false }),
	dpi: Object.freeze({ id: 0x04, length: 0x38, targeted: true }),
	buttons: Object.freeze({ id: 0x08, length: 0x3b, targeted: true }),
});

export type ReadbackReportName = keyof typeof READBACK_REPORTS;

export interface ReportValidation {
	readonly valid: boolean;
	readonly errors: readonly string[];
}

export interface TimingSummary {
	readonly count: number;
	readonly minimum: number;
	readonly p50: number;
	readonly p90: number;
	readonly p95: number;
	readonly p99: number;
	readonly maximum: number;
	readonly mean: number;
}

export function buildBenchmarkSelector(name: ReadbackReportName, targetProfile: number = 1): Buffer {
	if (!Number.isInteger(targetProfile) || targetProfile < 1 || targetProfile > 5) {
		throw new ParamsError('targetProfile', `Expected an integer from 1 to 5, got ${targetProfile}`);
	}
	const report = READBACK_REPORTS[name];
	return Buffer.from([0xa0, report.id, report.length, 0x00, targetProfile, 0x00, 0x00, 0x00]);
}

export function validateReadback(
	name: ReadbackReportName,
	packet: Buffer,
	targetProfile: number = 1,
): ReportValidation {
	const report = READBACK_REPORTS[name];
	const errors: string[] = [];
	if (packet.length !== report.length) errors.push(`length ${packet.length}, expected ${report.length}`);
	if (packet[0] !== report.id)
		errors.push(`report ID 0x${packet[0]?.toString(16) ?? 'missing'}, expected 0x${report.id.toString(16)}`);
	if (packet[1] !== report.length)
		errors.push(`length byte 0x${packet[1]?.toString(16) ?? 'missing'}, expected 0x${report.length.toString(16)}`);
	if (packet.length !== report.length) return { valid: false, errors };

	if (name === 'version' && packet[2] !== 0x01) {
		errors.push(`version prefix 0x${packet[2]?.toString(16)}, expected 0x1`);
	}
	if (name === 'profileMetadata') {
		if (packet[2] !== 0x01) errors.push(`metadata subtype 0x${packet[2]?.toString(16)}, expected 0x1`);
		if (((packet.readUInt8(3) + packet.readUInt8(4)) & 0xff) !== 0xff)
			errors.push('current-profile complement mismatch');
		if (((packet.readUInt8(5) + packet.readUInt8(6)) & 0xff) !== 0xff)
			errors.push('maximum-profile complement mismatch');
	}
	if (report.targeted && packet[2] !== targetProfile) {
		errors.push(`target profile ${packet[2]}, expected ${targetProfile}`);
	}
	if (name === 'dpi') {
		let checksum = 0;
		for (let index = 3; index <= 49; index++) checksum = (checksum + packet.readUInt8(index)) & 0xffff;
		if (packet.readUInt16BE(50) !== checksum) errors.push('DPI checksum mismatch');
	}
	if (name === 'buttons') {
		let checksum = 0;
		for (let index = 3; index <= 56; index++) checksum = (checksum + packet.readUInt8(index)) & 0xffff;
		if (packet.readUInt16BE(57) !== checksum) errors.push('button checksum mismatch');
	}
	return { valid: errors.length === 0, errors };
}

export function summarizeMicroseconds(values: readonly number[]): TimingSummary {
	if (values.length === 0) throw new ParamsError('values', 'Expected at least one timing sample');
	const sorted = values.toSorted((left, right) => left - right);
	const minimum = sorted[0];
	if (minimum === undefined) throw new ParamsError('values', 'Expected at least one timing sample');
	const percentile = (fraction: number): number => sorted[Math.ceil(fraction * sorted.length) - 1] ?? minimum;
	return {
		count: sorted.length,
		minimum,
		p50: percentile(0.5),
		p90: percentile(0.9),
		p95: percentile(0.95),
		p99: percentile(0.99),
		maximum: sorted.at(-1) ?? minimum,
		mean: Math.round(sorted.reduce((sum, value) => sum + value, 0) / sorted.length),
	};
}
