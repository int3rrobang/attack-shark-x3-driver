import { describe, expect, it } from 'bun:test';
import {
	Button,
	ConnectionMode,
	ParamsError,
	FirmwareAction,
	MacroName,
	MacrosBuilder,
	macroTemplates,
} from '../src/index.js';

const X3_WIRED_STOCK_DEFAULT_PACKET =
	'083b010200000300000400000d00003c00000f00000600000500003c00000100000100000100000100000100000100000100000a000009000000c2';

const x3WiredStockDefaultBytes =
	X3_WIRED_STOCK_DEFAULT_PACKET.match(/../g)?.map((byte) => Number.parseInt(byte, 16)) ?? [];

describe('MacrosBuilder', () => {
	it('should initialize with default buffer', () => {
		const builder = new MacrosBuilder();
		// Check header
		expect(builder.buffer[0]).toBe(0x08);
		expect(builder.buffer[1]).toBe(0x3b);
		expect(builder.buffer[2]).toBe(0x01);

		// Check default buttons
		// Left (index 3) -> 0x02
		expect(builder.buffer[3]).toBe(0x02);
		// Right (index 6) -> 0x03
		expect(builder.buffer[6]).toBe(0x03);
		// Middle (index 9) -> 0x04
		expect(builder.buffer[9]).toBe(0x04);
		// Forward (index 21) -> 0x06
		expect(builder.buffer[21]).toBe(0x06);
		// Backward (index 24) -> 0x05
		expect(builder.buffer[24]).toBe(0x05);

		// Check checksum (default)
		builder.build(ConnectionMode.Wired);
		expect(builder.buffer[58]).toBe(0x3e);
	});

	it('should set a macro correctly', () => {
		const builder = new MacrosBuilder();
		const macro = macroTemplates[MacroName.SHORTCUT_COPY]; // [FirmwareAction.KEYBOARD, Modifiers.CTRL, KeyCode.C]

		builder.setMacro(Button.LEFT, macro);

		expect(builder.buffer[3]).toBe(0x11); // KEYBOARD
		expect(builder.buffer[4]).toBe(0x01); // CTRL
		expect(builder.buffer[5]).toBe(0x06); // C
	});

	it('should calculate checksum correctly', () => {
		const builder = new MacrosBuilder();
		// Manual calculation for default buffer:
		// Header: 0x08, 0x3b, 0x01
		// sum = 0x01 (starts at index 2)
		// ... (all other bytes)
		// Default sum results in 0x3e as per previous test
		expect(builder.calculateChecksum()).toBe(0x3e);
	});

	it('should build stock X3 wired defaults', () => {
		const buffer = new MacrosBuilder().build(ConnectionMode.X3Wired);

		expect(buffer.toString('hex')).toBe(X3_WIRED_STOCK_DEFAULT_PACKET);
		expect(buffer[51]).toBe(0x0a);
		expect(buffer[54]).toBe(0x09);
		expect(buffer[58]).toBe(0xc2);
	});

	it('should preserve X3 wired scroll defaults when disabling forward', () => {
		const buffer = new MacrosBuilder()
			.setMacro(Button.FORWARD, macroTemplates[MacroName.GLOBAL_DISABLE_BUTTON])
			.build(ConnectionMode.X3Wired);
		const changedOffsets = [...buffer.entries()]
			.filter(([offset, value]) => x3WiredStockDefaultBytes[offset] !== value)
			.map(([offset]) => offset);

		expect(changedOffsets).toEqual([21, 58]);
		expect(buffer[21]).toBe(0x01);
		expect(buffer[22]).toBe(0x00);
		expect(buffer[23]).toBe(0x00);
		expect(buffer[51]).toBe(0x0a);
		expect(buffer[54]).toBe(0x09);
		expect(buffer[58]).toBe(0xbd);
	});

	it('should map logical X3 wired scroll-up overrides to offset 54', () => {
		const buffer = new MacrosBuilder()
			.setMacro(Button.SCROLL_UP, macroTemplates[MacroName.SHORTCUT_SWAP_WINDOW])
			.build(ConnectionMode.X3Wired);

		expect(buffer[51]).toBe(0x0a);
		expect(buffer[52]).toBe(0x00);
		expect(buffer[53]).toBe(0x00);
		expect(buffer[54]).toBe(FirmwareAction.KEYBOARD);
		expect(buffer[55]).toBe(0x04);
		expect(buffer[56]).toBe(0x2b);
		expect(buffer[58]).toBe(0xf9);
	});

	it('should map logical X3 wired scroll-down overrides to offset 51', () => {
		const buffer = new MacrosBuilder()
			.setMacro(Button.SCROLL_DOWN, macroTemplates[MacroName.GLOBAL_DISABLE_BUTTON])
			.build(ConnectionMode.X3Wired);

		expect(buffer[51]).toBe(FirmwareAction.DISABLE_BUTTON);
		expect(buffer[52]).toBe(0x00);
		expect(buffer[53]).toBe(0x00);
		expect(buffer[54]).toBe(0x09);
		expect(buffer[55]).toBe(0x00);
		expect(buffer[56]).toBe(0x00);
		expect(buffer[58]).toBe(0xb9);
	});

	it('should keep X11 scroll override offsets unchanged', () => {
		const buffer = new MacrosBuilder()
			.setMacro(Button.SCROLL_UP, macroTemplates[MacroName.MULTIMEDIA_VOLUME_PLUS])
			.setMacro(Button.SCROLL_DOWN, macroTemplates[MacroName.MULTIMEDIA_VOLUME_MINUS])
			.build(ConnectionMode.Wired);

		expect(buffer[51]).toBe(FirmwareAction.VOL_PLUS);
		expect(buffer[54]).toBe(FirmwareAction.VOL_MINUS);
		expect(buffer[58]).toBe(0x62);
	});

	it('should support method chaining', () => {
		const builder = new MacrosBuilder();
		const result = builder.setMacro(Button.LEFT, macroTemplates[MacroName.GLOBAL_LEFT_CLICK]);
		expect(result).toBe(builder);
	});

	it('should support new descriptive button names', () => {
		const builder = new MacrosBuilder();
		builder.setMacro(Button.LEFT, macroTemplates[MacroName.GLOBAL_LEFT_CLICK]);
		builder.setMacro(Button.FORWARD, macroTemplates[MacroName.GLOBAL_FORWARD]);

		expect(builder.buffer[3]).toBe(0x02); // Left-Click
		expect(builder.buffer[21]).toBe(0x06); // Forward
	});

	it('should support remapping DPI button', () => {
		const builder = new MacrosBuilder();
		// Remap the DPI button (index 18) to Middle-Click
		builder.setMacro(Button.DPI, macroTemplates[MacroName.GLOBAL_MIDDLE]);

		expect(builder.buffer[18]).toBe(0x04); // MIDDLE_CLICK
		expect(builder.buffer[19]).toBe(0x00);
		expect(builder.buffer[20]).toBe(0x00);
	});

	it('should support remapping scroll wheel', () => {
		const builder = new MacrosBuilder();
		builder.setMacro(
			Button.SCROLL_UP,
			macroTemplates[MacroName.MULTIMEDIA_VOLUME_PLUS] ?? [FirmwareAction.VOL_PLUS, 0, 0],
		);
		builder.setMacro(
			Button.SCROLL_DOWN,
			macroTemplates[MacroName.MULTIMEDIA_VOLUME_MINUS] ?? [FirmwareAction.VOL_MINUS, 0, 0],
		);

		expect(builder.buffer[51]).toBe(0x1b); // VOL_PLUS
		expect(builder.buffer[54]).toBe(0x1c); // VOL_MINUS
	});

	it('should throw error for invalid button identifier', () => {
		const builder = new MacrosBuilder();
		// @ts-expect-error test
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		expect(() => builder.setMacro(99 as any, [0, 0, 0])).toThrow(ParamsError);
	});

	it('should initialize with custom options in constructor', () => {
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		const customMacro: any = [0x11, 0x01, 0x04]; // Keyboard, Ctrl, A
		const builder = new MacrosBuilder({
			left: customMacro,
			forward: macroTemplates[MacroName.GLOBAL_FIRE_BUTTON],
			dpi: macroTemplates[MacroName.GLOBAL_DPI_PLUS],
		});

		expect(builder.buffer[3]).toBe(0x11); // Left remapped
		expect(builder.buffer[21]).toBe(0x08); // Forward remapped to FIRE
		expect(builder.buffer[18]).toBe(0x0e); // DPI remapped to DPI+
	});
});
