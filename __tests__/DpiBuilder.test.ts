import { describe, expect, it } from 'bun:test';
import { DpiBuilder } from '../src/protocols/DpiBuilder.js';
import { ParamsError } from '../src/errors.js';
import { TransportKind } from '../src/types.js';

describe('DpiBuilder', () => {
	it('builds the profile-1 wired FA61 fixture', () => {
		const buffer = new DpiBuilder().build(TransportKind.Wired);
		expect(buffer.toString('hex')).toBe(
			'04380100003f00000f1f2f3f63070000000000000002000002ff000000ff000000ffffff0000ffffff00ffff4000ffffff010e7d',
		);
		expect(buffer.length).toBe(52);
	});

	it('pads the receiver packet without changing its wired payload', () => {
		const builder = new DpiBuilder();
		const wired = builder.build(TransportKind.Wired);
		const receiver = builder.build(TransportKind.Receiver);
		expect(receiver.length).toBe(56);
		expect(receiver.subarray(0, 52)).toEqual(wired);
		expect([...receiver.subarray(52)]).toEqual([0, 0, 0, 0]);
	});

	it('uses direct low/high encoding for the supported DPI range', () => {
		const builder = new DpiBuilder({ dpiValues: [50, 12_800, 26_000] });
		const buffer = builder.build(TransportKind.Wired);
		expect([...buffer.subarray(8, 11)]).toEqual([0x00, 0xff, 0x07]);
		expect([...buffer.subarray(16, 19)]).toEqual([0x00, 0x00, 0x02]);
	});

	it('rejects values outside the 50-step DPI range', () => {
		expect(() => new DpiBuilder({ dpiValues: [51] })).toThrow(ParamsError);
		expect(() => new DpiBuilder({ dpiValues: [26_050] })).toThrow(ParamsError);
	});

	it('supports one through eight stages and clears disabled slots', () => {
		const three = new DpiBuilder({ dpiValues: [400, 800, 1600], activeStage: 3 }).build(TransportKind.Wired);
		expect(three[5]).toBe(0x07);
		expect([...three.subarray(11, 16)]).toEqual([0, 0, 0, 0, 0]);

		const eight = new DpiBuilder({
			dpiValues: [400, 800, 1600, 2400, 3200, 5000, 20_400, 26_000],
			activeStage: 8,
		}).build(TransportKind.Wired);
		expect(eight[5]).toBe(0xff);
		expect(eight[24]).toBe(0x08);
		expect([...eight.subarray(14, 16)]).toEqual([0x97, 0x07]);
		expect([...eight.subarray(22, 24)]).toEqual([0x01, 0x02]);
	});

	it('writes X3 sensor options at their protocol offsets', () => {
		const buffer = new DpiBuilder({ lod: 2, ripplerControl: true, angleSnap: true, motionSync: true }).build(
			TransportKind.Wired,
		);
		expect([...buffer.subarray(3, 8)]).toEqual([1, 1, 0x3f, 1, 1]);
	});

	it('validates the active stage against the configured stages', () => {
		expect(() =>
			new DpiBuilder({ dpiValues: [400, 800, 1600], activeStage: 4 }).build(TransportKind.Wired),
		).toThrow(ParamsError);
	});

	it('writes the 16-bit big-endian checksum over bytes 3 through 49', () => {
		const builder = new DpiBuilder();
		builder.build(TransportKind.Wired);
		expect(builder.buffer.readUInt16BE(50)).toBe(builder.calculateChecksum());
	});
});
