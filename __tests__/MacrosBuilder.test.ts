import { describe, expect, it } from 'bun:test';
import { ParamsError } from '../src/errors.js';
import { Button, TransportKind } from '../src/types.js';
import {
	FirmwareAction,
	KeyCode,
	MacroName,
	MacrosBuilder,
	Modifiers,
	macroTemplates,
} from '../src/protocols/MacrosBuilder.js';

const STOCK_PACKET =
	'083b010200000300000400000d00003c00000f00000600000500003c00000100000100000100000100000100000100000100000a000009000000c2';

describe('MacrosBuilder', () => {
	it('initializes from the profile-1 X3 layout', () => {
		const buffer = new MacrosBuilder().build(TransportKind.Wired);
		expect(buffer.toString('hex')).toBe(STOCK_PACKET);
		expect(buffer.length).toBe(59);
	});

	it('uses the same X3 packet for either transport', () => {
		const builder = new MacrosBuilder();
		const wired = builder.build(TransportKind.Wired);
		const receiver = builder.build(TransportKind.Receiver);
		expect(receiver).toEqual(wired);
	});

	it('writes a 16-bit big-endian checksum over bytes 3 through 56', () => {
		const builder = new MacrosBuilder();
		builder.build(TransportKind.Wired);
		expect(builder.buffer.readUInt16BE(57)).toBe(builder.calculateChecksum());
	});

	it('retains the X3 scroll slot order', () => {
		const buffer = new MacrosBuilder()
			.setMacro(Button.SCROLL_UP, macroTemplates[MacroName.SHORTCUT_SWAP_WINDOW])
			.setMacro(Button.SCROLL_DOWN, macroTemplates[MacroName.GLOBAL_DISABLE_BUTTON])
			.build(TransportKind.Wired);
		expect([...buffer.subarray(51, 57)]).toEqual([
			FirmwareAction.DISABLE_BUTTON,
			0,
			0,
			FirmwareAction.KEYBOARD,
			Modifiers.ALT,
			KeyCode.TAB,
		]);
	});

	it('supports button, DPI, and constructor overrides', () => {
		const buffer = new MacrosBuilder({
			left: [FirmwareAction.KEYBOARD, Modifiers.CTRL, KeyCode.C],
			forward: macroTemplates[MacroName.GLOBAL_FIRE_BUTTON],
			dpi: macroTemplates[MacroName.GLOBAL_DPI_PLUS],
		}).build(TransportKind.Wired);
		expect([...buffer.subarray(3, 6)]).toEqual([FirmwareAction.KEYBOARD, Modifiers.CTRL, KeyCode.C]);
		expect(buffer[21]).toBe(FirmwareAction.FIRE);
		expect(buffer[18]).toBe(FirmwareAction.GLOBAL_DPI_PLUS);
	});

	it('rejects an invalid button identifier', () => {
		const builder = new MacrosBuilder();
		expect(() => builder.setMacro(99 as Button, [0, 0, 0])).toThrow(ParamsError);
	});
});
