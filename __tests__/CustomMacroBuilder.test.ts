import { describe, expect, it } from 'bun:test';
import { Button, TransportKind } from '../src/types.js';
import { KeyCode, MacrosBuilder, FirmwareAction } from '../src/protocols/MacrosBuilder.js';
import { CustomMacroBuilder, MacroMode } from '../src/protocols/CustomMacroBuilder.js';

describe('CustomMacroBuilder', () => {
	it.each([
		[0, 1],
		[10, 1],
		[20, 3],
		[500, 51],
	])('encodes %d ms as event delay %d', (delay, expected) => {
		const [, page0] = new CustomMacroBuilder().addEvent(KeyCode.A, delay).build(TransportKind.Wired);
		expect(page0[30]).toBe(expected);
		expect(page0[31]).toBe(KeyCode.A);
	});

	it('encodes long delays with an extra delay event', () => {
		const [, page0] = new CustomMacroBuilder().addEvent(KeyCode.A, 5000).build(TransportKind.Receiver);
		expect([...page0.subarray(30, 34)]).toEqual([1, KeyCode.A, 0x19, 3]);
	});

	it('uses the X3 button builder for the bind packet', () => {
		const [bindPacket] = new CustomMacroBuilder({ targetButton: Button.FORWARD }).build(TransportKind.Wired);
		expect(bindPacket[21]).toBe(FirmwareAction.CUSTOM_MACRO);
		expect(bindPacket[22]).toBe(0);
		expect(bindPacket[23]).toBe(0x07);
	});

	it('uses page-2 header 09 40 for every transport', () => {
		for (const transport of [TransportKind.Wired, TransportKind.Receiver]) {
			const [, , , page2] = new CustomMacroBuilder({
				targetButton: Button.LEFT,
				playOptions: { mode: MacroMode.THE_NUMBER_OF_TIME_TO_PLAY, times: 2 },
			}).build(transport);
			expect(page2.subarray(0, 4)).toEqual(Buffer.from([0x09, 0x40, 0x01, 0x02]));
		}
	});

	it('writes the event count and packet checksum', () => {
		const builder = new CustomMacroBuilder().addEvent(KeyCode.A).addEvent(KeyCode.A, 20, true);
		const [, page0, page1, page2] = builder.build(TransportKind.Wired);
		expect(page0[29]).toBe(2);
		expect(page0[30]).toBe(1);
		expect(page0[32]).toBe(0x83);
		expect(page1[4]).toBe(0);
		expect(page2.readUInt16BE(10)).toBe(builder.calculateChecksum());
	});

	it('can use an existing macro layout', () => {
		const macros = new MacrosBuilder().setMacro(Button.LEFT, [FirmwareAction.MIDDLE_CLICK, 0, 0]);
		const [bindPacket] = new CustomMacroBuilder({ targetButton: Button.LEFT, macrosBuilder: macros }).build(
			TransportKind.Wired,
		);
		expect(bindPacket[3]).toBe(FirmwareAction.CUSTOM_MACRO);
	});
});
