import { describe, expect, it } from 'bun:test';
import {
	buildBenchmarkSelector,
	summarizeMicroseconds,
	validateReadback,
} from '../src/experimental/Fa61ReadbackBenchmark.js';

const DPI = Buffer.from(
	'04380100000100001f00000000000000000000000000000001ff000000ff000000ffffff0000ffffff00ffff4000ffffff010d5500000000',
	'hex',
);
const BUTTONS = Buffer.from(
	'083b010200000300000400000d00003c00000f00000600000500003c00000100000100000100000100000100000100000100000a000009000000c2',
	'hex',
);

describe('FA61 readback benchmark helpers', () => {
	it('builds report selectors with explicit targets', () => {
		expect(buildBenchmarkSelector('dpi', 1).toString('hex')).toBe('a004380001000000');
		expect(buildBenchmarkSelector('buttons', 2).toString('hex')).toBe('a0083b0002000000');
		expect(() => buildBenchmarkSelector('dpi', 0)).toThrow('Expected an integer from 1 to 5');
	});

	it('validates known profile metadata, DPI, and button packets', () => {
		expect(validateReadback('profileMetadata', Buffer.from('0c0a0101fe01fe000000', 'hex')).valid).toBe(true);
		expect(validateReadback('dpi', DPI, 1).valid).toBe(true);
		expect(validateReadback('buttons', BUTTONS, 1).valid).toBe(true);
	});

	it('rejects mixed report IDs, targets, and checksums', () => {
		const mixed = Buffer.from(DPI);
		mixed[0] = 0x0c;
		expect(validateReadback('dpi', mixed, 1).errors).toContain('report ID 0xc, expected 0x4');

		expect(validateReadback('version', Buffer.from('0b08021070830105', 'hex')).errors).toContain(
			'version prefix 0x2, expected 0x1',
		);

		const wrongTarget = Buffer.from(DPI);
		wrongTarget[2] = 0x02;
		expect(validateReadback('dpi', wrongTarget, 1).errors).toContain('target profile 2, expected 1');

		const wrongChecksum = Buffer.from(BUTTONS);
		wrongChecksum[58] ^= 0x01;
		expect(validateReadback('buttons', wrongChecksum, 1).errors).toContain('button checksum mismatch');
	});

	it('summarizes nearest-rank latency percentiles', () => {
		expect(summarizeMicroseconds([1, 2, 3, 4, 100])).toEqual({
			count: 5,
			minimum: 1,
			p50: 3,
			p90: 100,
			p95: 100,
			p99: 100,
			maximum: 100,
			mean: 22,
		});
	});
});
