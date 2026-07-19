import { describe, expect, it } from 'bun:test';
import {
	buildButtonAction,
	buildProfileSelector,
	buildReadSelector,
	buildSingleStageDpi,
	calculateSectionChecksum,
	decodeFirstDpi,
	decodeProfileMetadata,
	hasLoadedDistinctProfile,
} from '../src/experimental/Fa61ProfileProbe.js';

const BASELINE_DPI = Buffer.from(
	'04380100003f00000f1f2f3f63070000000000000002000001ff000000ff000000ffffff0000ffffff00ffff4000ffffff010e7c00000000',
	'hex',
);
const BASELINE_BUTTONS = Buffer.from(
	'083b010200000300000400000d00003c00000f00001200070500003c00000100000100000100000100000100000100000100000a000009000000d5',
	'hex',
);

describe('FA61 profile probe packets', () => {
	it('builds explicit target-profile A0 selectors', () => {
		expect(buildReadSelector(0x04, 0x38, 1).toString('hex')).toBe('a004380001000000');
		expect(buildReadSelector(0x04, 0x38, 2).toString('hex')).toBe('a004380002000000');
		expect(() => buildReadSelector(0x04, 0x38, 0)).toThrow('one-based profile');
	});

	it('builds complemented six-byte profile selectors', () => {
		expect(buildProfileSelector(1, 5).toString('hex')).toBe('0c0a01fe05fa');
		expect(buildProfileSelector(2, 5).toString('hex')).toBe('0c0a02fd05fa');
		expect(() => buildProfileSelector(2, 1)).toThrow('1 <= current <= maximum <= 5');
		expect(() => buildProfileSelector(1, 6)).toThrow('1 <= current <= maximum <= 5');
	});

	it('decodes normalized profile metadata and rejects bad complements', () => {
		expect(decodeProfileMetadata(Buffer.from('0c0a0102fd05fa000000', 'hex'))).toEqual({ current: 2, maximum: 5 });
		expect(() => decodeProfileMetadata(Buffer.from('0c0a01020005fa000000', 'hex'))).toThrow('invalid complement');
	});

	it('patches one DPI stage without mutating the baseline', () => {
		const baselineHex = BASELINE_DPI.toString('hex');
		const packet = buildSingleStageDpi(BASELINE_DPI, 3200, 2);

		expect(BASELINE_DPI.toString('hex')).toBe(baselineHex);
		expect(packet[2]).toBe(0x02);
		expect(packet[5]).toBe(0x01);
		expect(packet.subarray(8, 16).toString('hex')).toBe('3f00000000000000');
		expect(packet.subarray(16, 24).toString('hex')).toBe('0000000000000000');
		expect(packet[24]).toBe(0x01);
		expect(decodeFirstDpi(packet)).toBe(3200);
		expect(packet.readUInt16BE(50)).toBe(calculateSectionChecksum(packet, 3, 49));
	});

	it('patches only the selected button entry and its checksum', () => {
		const baselineHex = BASELINE_BUTTONS.toString('hex');
		const packet = buildButtonAction(BASELINE_BUTTONS, 21, 0x34, 2);

		expect(BASELINE_BUTTONS.toString('hex')).toBe(baselineHex);
		expect(packet[2]).toBe(0x02);
		expect(packet.subarray(21, 24).toString('hex')).toBe('340000');
		expect(packet.subarray(3, 21)).toEqual(BASELINE_BUTTONS.subarray(3, 21));
		expect(packet.subarray(24, 57)).toEqual(BASELINE_BUTTONS.subarray(24, 57));
		expect(packet.readUInt16BE(57)).toBe(calculateSectionChecksum(packet, 3, 56));
	});

	it('blocks profile-two writes until metadata and live DPI both change', () => {
		const profile1Dpi = buildSingleStageDpi(BASELINE_DPI, 800, 1).toString('hex');
		const profile2Dpi = buildSingleStageDpi(BASELINE_DPI, 3200, 2).toString('hex');
		const profile1BodyWithProfile2Selector = Buffer.from(profile1Dpi, 'hex');
		profile1BodyWithProfile2Selector[2] = 0x02;

		expect(
			hasLoadedDistinctProfile(
				{ metadata: { current: 2, maximum: 5 }, profileMetadataHex: '', dpiHex: profile1Dpi },
				2,
				profile1Dpi,
			),
		).toBe(false);
		expect(
			hasLoadedDistinctProfile(
				{ metadata: { current: 1, maximum: 5 }, profileMetadataHex: '', dpiHex: profile2Dpi },
				2,
				profile1Dpi,
			),
		).toBe(false);
		expect(
			hasLoadedDistinctProfile(
				{
					metadata: { current: 2, maximum: 5 },
					profileMetadataHex: '',
					dpiHex: profile1BodyWithProfile2Selector.toString('hex'),
				},
				2,
				profile1Dpi,
			),
		).toBe(false);
		expect(
			hasLoadedDistinctProfile(
				{ metadata: { current: 2, maximum: 5 }, profileMetadataHex: '', dpiHex: profile2Dpi },
				2,
				profile1Dpi,
			),
		).toBe(true);
	});
});
