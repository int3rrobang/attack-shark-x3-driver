import { describe, expect, it } from 'bun:test';
import { ConnectionMode, DpiBuilder, ParamsError } from '../src/index.js';

describe('DpiBuilder', () => {
	it('should initialize with default buffer (X11)', () => {
		const builder = new DpiBuilder();
		// Default: Angle Snap Off (0x00), Rippler On (0x01), Stages: 800, 1600, 2400, 3200, 5000, 22000
		expect(builder.toString()).toBe(
			'04380100013f20201225384b75810000000000000001000002ff000000ff000000ffffff0000ffffff00ffff4000ffffff020f6800000000',
		);
	});

	it('should produce X3 fixed bytes and checksum when built for X3Wired', () => {
		const builder = new DpiBuilder(DpiBuilder.X3_DEFAULT_OPTIONS);
		const buffer = builder.build(ConnectionMode.X3Wired);
		expect(buffer.toString('hex')).toBe(
			'04380100003f00000f1f2f3f63070000000000000002000002ff000000ff000000ffffff0000ffffff00ffff4000ffffff010e7d',
		);
	});

	it('should restore X11 fixed bytes and checksum when building X3 then X11 on same builder', () => {
		const builder = new DpiBuilder();
		// Build once as X3Wired
		builder.build(ConnectionMode.X3Wired);
		// Build again as Wired — must restore X11 fixed bytes + recalc checksum
		builder.build(ConnectionMode.Wired);
		// Should match a fresh builder built only as Wired
		const fresh = new DpiBuilder();
		fresh.build(ConnectionMode.Wired);
		expect(builder.toString()).toBe(fresh.toString());
	});

	it('should have correct USB control transfer parameters', () => {
		const builder = new DpiBuilder();
		expect(builder.bmRequestType).toBe(0x21);
		expect(builder.bRequest).toBe(0x09);
		expect(builder.wValue).toBe(0x0304);
		expect(builder.wIndex).toBe(2);
	});

	it('should set Angle Snap and Rippler Control', () => {
		const builder = new DpiBuilder();

		builder.setAngleSnap(true);
		expect(builder.buffer[3]).toBe(0x01);

		builder.setAngleSnap(false);
		expect(builder.buffer[3]).toBe(0x00);

		builder.setRipplerControl(true);
		expect(builder.buffer[4]).toBe(0x01);

		builder.setRipplerControl(false);
		expect(builder.buffer[4]).toBe(0x00);
	});

	it('should set current stage', () => {
		const builder = new DpiBuilder();
		builder.setCurrentStage(4);
		expect(builder.buffer[24]).toBe(0x04);
	});

	it('should set DPI values and encode them correctly', () => {
		const builder = new DpiBuilder();

		// 800 DPI -> 0x12
		builder.setDpiValue(1, 800);
		expect(builder.buffer[8]).toBe(0x12);

		// 1600 DPI -> 0x25
		builder.setDpiValue(2, 1600);
		expect(builder.buffer[9]).toBe(0x25);

		// Test throw for unsupported DPI
		expect(() => builder.setDpiValue(1, 99999)).toThrow(ParamsError);
	});

	it('should update stage mask and high stage flags during build', () => {
		const builder = new DpiBuilder();

		// Default stages: 800, 1600, 2400, 3200, 5000, 22,000
		// 22,000 is in range [20,100, 22,000] (X11), so the high flag should be 1
		// 22,000 is > 12,000, so mask bit 5 (0x20) should be set

		builder.build(ConnectionMode.Wired);

		// Stage 6 (index 5) is 22,000
		expect(builder.buffer[21]).toBe(0x01); // High flag stage 6
		expect(builder.buffer[20]).toBe(0x00); // 5000 is not in ranges

		// 22,000 is > 12,000, so the mask should be 0x20 (bit 5)
		expect(builder.buffer[6]).toBe(0x20);
		expect(builder.buffer[7]).toBe(0x20);

		// Test Range A: 10,100 - 12,000
		builder.setDpiValue(1, 10100);
		builder.build(ConnectionMode.Wired);
		expect(builder.buffer[16]).toBe(0x01); // High flag stage 1 active
		expect(builder.buffer[6]).toBe(0x20); // Mask stage 1 NOT active (10,100 <= 12,000)

		// Test Range B: 20100 - 22000 (X11)
		builder.setDpiValue(2, 20500);
		builder.build(ConnectionMode.Wired);
		expect(builder.buffer[17]).toBe(0x01); // High flag stage 2 active
		expect(builder.buffer[6]).toBe(0x22); // Mask stage 2 active (20500 > 12000) | stage 6 (0x20) = 0x22

		// Test value between ranges: 15,000
		builder.setDpiValue(3, 15000);
		builder.build(ConnectionMode.Wired);
		expect(builder.buffer[18]).toBe(0x00); // High flag stage 3 NOT active
		expect(builder.buffer[6]).toBe(0x26); // Mask stage 3 active (15,000 > 12,000) | 0x22 = 0x26
	});

	it('should use captured low/high DPI encoding for X3 up to 26000', () => {
		const builder = new DpiBuilder();
		// 24,000 exceeds X11's 22,000 upper limit, but X3's limit is 26,000
		builder.setDpiValue(6, 24000);
		builder.build(ConnectionMode.X3Wired);
		expect(builder.buffer[13]).toBe(0xdf); // 24000 / 50 - 1 low byte
		expect(builder.buffer[21]).toBe(0x01); // 24000 / 50 - 1 high byte
		expect(builder.buffer[6]).toBe(0x00); // X3 angle snap, not X11 high-DPI mask

		// Same value built as X11 should be rejected before high flags are calculated
		const builder2 = new DpiBuilder();
		builder2.setDpiValue(6, 24000);
		expect(() => builder2.build(ConnectionMode.Wired)).toThrow(ParamsError);
	});

	it('should calculate correct checksum', () => {
		const builder = new DpiBuilder();
		builder.build(ConnectionMode.Adapter);
		// Sum of buffer from index 3 to 49 AFTER build (which updates masks/flags)
		const checksum = builder.calculateChecksum();

		expect(builder.buffer[50]).toBe((checksum >> 8) & 0xff);
		expect(builder.buffer[51]).toBe(checksum & 0xff);
	});

	it('should return correct buffer size for Adapter vs Wired mode', () => {
		const builder = new DpiBuilder();

		const wiredBuffer = builder.build(ConnectionMode.Wired);
		expect(wiredBuffer.length).toBe(52); // indices 0 to 51

		const adapterBuffer = builder.build(ConnectionMode.Adapter);
		expect(adapterBuffer.length).toBe(56);

		// X3Wired treated as wired → 52 bytes
		const x3Builder = new DpiBuilder(DpiBuilder.X3_DEFAULT_OPTIONS);
		const x3WiredBuffer = x3Builder.build(ConnectionMode.X3Wired);
		expect(x3WiredBuffer.length).toBe(52);
	});

	it('should reject DPI > 22000 for non-X3 modes', () => {
		const builder = new DpiBuilder({ dpiValues: [800, 1600, 2400, 3200, 5000, 26000] });
		expect(() => builder.build(ConnectionMode.Wired)).toThrow(ParamsError);
	});

	it('should allow DPI up to 26000 for X3Wired', () => {
		const builder = new DpiBuilder({ dpiValues: [800, 1600, 2400, 3200, 5000, 26000] });
		const buffer = builder.build(ConnectionMode.X3Wired);
		expect(buffer.length).toBe(52);
	});

	it('should build X3 packet with 3 enabled stages and zero disabled stage bytes', () => {
		const builder = new DpiBuilder({ dpiValues: [400, 800, 1600], activeStage: 3 });
		const buffer = builder.build(ConnectionMode.X3Wired);

		expect(buffer.length).toBe(52);
		expect(buffer[5]).toBe(0x07);
		expect([...buffer.subarray(8, 16)]).toEqual([0x07, 0x0f, 0x1f, 0x00, 0x00, 0x00, 0x00, 0x00]);
		expect([...buffer.subarray(16, 24)]).toEqual([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
		expect(buffer[24]).toBe(0x03);
	});

	it('should build X3 packet with captured 400-1800 DPI encoding', () => {
		const builder = new DpiBuilder({
			...DpiBuilder.X3_DEFAULT_OPTIONS,
			dpiValues: [400, 450, 500, 1600, 1700, 1800],
		});
		const buffer = builder.build(ConnectionMode.X3Wired);

		expect(buffer.toString('hex')).toBe(
			'04380100003f00000708091f21230000000000000000000002ff000000ff000000ffffff0000ffffff00ffff4000ffffff010df0',
		);
	});

	it('should encode X3 sensor toggles at captured offsets', () => {
		const defaults = DpiBuilder.X3_DEFAULT_OPTIONS;
		expect([...new DpiBuilder({ ...defaults, lod: 2 }).build(ConnectionMode.X3Wired).subarray(3, 8)]).toEqual([
			0x01, 0x00, 0x3f, 0x00, 0x00,
		]);
		expect([
			...new DpiBuilder({ ...defaults, ripplerControl: true }).build(ConnectionMode.X3Wired).subarray(3, 8),
		]).toEqual([0x00, 0x01, 0x3f, 0x00, 0x00]);
		expect([
			...new DpiBuilder({ ...defaults, angleSnap: true }).build(ConnectionMode.X3Wired).subarray(3, 8),
		]).toEqual([0x00, 0x00, 0x3f, 0x01, 0x00]);
		expect([
			...new DpiBuilder({ ...defaults, motionSync: true }).build(ConnectionMode.X3Wired).subarray(3, 8),
		]).toEqual([0x00, 0x00, 0x3f, 0x00, 0x01]);
	});

	it('should build X3 packet with 8 stages using stage 7 and 8 bytes and high flags', () => {
		const builder = new DpiBuilder({
			dpiValues: [400, 800, 1600, 2400, 3200, 5000, 20400, 26000],
			activeStage: 8,
		});
		const buffer = builder.build(ConnectionMode.X3Wired);

		expect(buffer.length).toBe(52);
		expect(buffer[5]).toBe(0xff);
		expect(buffer[14]).toBe(0x97);
		expect(buffer[15]).toBe(0x07);
		expect(buffer[22]).toBe(0x01);
		expect(buffer[23]).toBe(0x02);
		expect(buffer[24]).toBe(0x08);
	});

	it('should validate X3 active stage against provided stage count', () => {
		expect(() =>
			new DpiBuilder({ dpiValues: [400, 800, 1600], activeStage: 4 }).build(ConnectionMode.X3Wired),
		).toThrow(ParamsError);
		expect(() =>
			new DpiBuilder({ dpiValues: [400, 800, 1600], activeStage: 3 }).build(ConnectionMode.X3Wired),
		).not.toThrow();
	});

	it('should require exactly 6 stages for X11 modes', () => {
		expect(() => new DpiBuilder({ dpiValues: [400, 800, 1600] }).build(ConnectionMode.Wired)).toThrow(ParamsError);
	});
});
