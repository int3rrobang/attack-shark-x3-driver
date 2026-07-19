import { describe, expect, it } from 'bun:test';
import { ParamsError } from '../src/errors.js';
import { LightMode, UserPreferencesBuilder } from '../src/protocols/UserPreferencesBuilder.js';
import { TransportKind } from '../src/types.js';

describe('UserPreferencesBuilder', () => {
	it('builds the profile-1 wired FA61 fixture', () => {
		const buffer = new UserPreferencesBuilder().build(TransportKind.Wired);
		expect(buffer.toString('hex')).toBe('050f010003a800ff00010201ad');
		expect(buffer.length).toBe(13);
	});

	it('returns the padded receiver packet with the same payload', () => {
		const builder = new UserPreferencesBuilder();
		const wired = builder.build(TransportKind.Wired);
		const receiver = builder.build(TransportKind.Receiver);
		expect(receiver.length).toBe(15);
		expect(receiver.subarray(0, 13)).toEqual(wired);
		expect([...receiver.subarray(13)]).toEqual([0, 0]);
	});

	it('calculates the confirmed 16-bit checksum over bytes 3 through 10', () => {
		const builder = new UserPreferencesBuilder({
			lightMode: LightMode.Static,
			rgb: { r: 0x12, g: 0x34, b: 0x56 },
			ledSpeed: 5,
			sleepTime: 10,
			deepSleepTime: 20,
			keyResponse: 20,
		});
		builder.build(TransportKind.Wired);
		expect(builder.buffer.readUInt16BE(11)).toBe(builder.calculateChecksum());
		expect(builder.buffer[11]).not.toBe(0);
	});

	it('updates configurable fields before calculating the checksum', () => {
		const builder = new UserPreferencesBuilder()
			.setLightMode(LightMode.BreathingDpi)
			.setDeepSleep(30)
			.setLedSpeed(1)
			.setRgb({ r: 1, g: 2, b: 3 })
			.setSleep(2)
			.setKeyResponse(10);
		const buffer = builder.build(TransportKind.Wired);
		expect([...buffer.subarray(3, 11)]).toEqual([0x60, 0x15, 0xe8, 1, 2, 3, 4, 5]);
		expect(buffer.readUInt16BE(11)).toBe(builder.calculateChecksum());
	});

	it('rejects invalid timer and response values', () => {
		expect(() => new UserPreferencesBuilder().setDeepSleep(0 as never)).toThrow(ParamsError);
		expect(() => new UserPreferencesBuilder().setSleep(30.5 as never)).toThrow(ParamsError);
		expect(() => new UserPreferencesBuilder().setKeyResponse(5 as never)).toThrow(ParamsError);
	});
});
