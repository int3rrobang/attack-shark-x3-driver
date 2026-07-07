import { expect, test, describe } from 'bun:test';
import { CustomMacroBuilder, MacroMode, MouseMacroEvent } from '../src/protocols/CustomMacroBuilder.js';
import { KeyCode, MacrosBuilder, FirmwareAction } from '../src/protocols/MacrosBuilder.js';
import { Button, ConnectionMode } from '../src/types.js';

describe('CustomMacroBuilder Delays', () => {
	test('Formula 2*floor((ms+5)/20)+1 should match samples', () => {
		const delays = [
			{ ms: 10, expected: 1 },
			{ ms: 15, expected: 3 },
			{ ms: 20, expected: 3 },
			{ ms: 35, expected: 5 },
			{ ms: 55, expected: 7 },
			{ ms: 75, expected: 9 },
			{ ms: 95, expected: 11 },
			{ ms: 110, expected: 11 },
			{ ms: 115, expected: 13 },
			{ ms: 255, expected: 27 },
		];

		for (const { ms, expected } of delays) {
			const customMacro = new CustomMacroBuilder().addEvent(KeyCode.A, ms);
			const [, secondPacket] = customMacro.build(ConnectionMode.Adapter);
			expect(secondPacket[30]).toBe(expected);
		}
	});

	test('Long delays should use extra units and remainder formula', () => {
		// 5000ms: extraUnits = 25 (0x19), rem = 0, byte = 1
		const customMacro = new CustomMacroBuilder().addEvent(KeyCode.A, 5000);
		const [, secondPacket] = customMacro.build(ConnectionMode.Adapter);

		// Event 1: [01, 04]
		// Event 2: [19, 03]
		expect(secondPacket[30]).toBe(0x01);
		expect(secondPacket[31]).toBe(KeyCode.A);
		expect(secondPacket[32]).toBe(0x19);
		expect(secondPacket[33]).toBe(0x03);
	});

	test('Mouse events should use the same formula as keyboard', () => {
		const customMacro = new CustomMacroBuilder()
			.addEvent(MouseMacroEvent.LEFT_CLICK, 20)
			.addEvent(MouseMacroEvent.LEFT_CLICK, 20, true);

		const [, secondPacket] = customMacro.build(ConnectionMode.Adapter);

		// 20ms -> 3
		expect(secondPacket[30]).toBe(0x03);
		expect(secondPacket[31]).toBe(MouseMacroEvent.LEFT_CLICK);
		expect(secondPacket[32]).toBe(0x83);
		expect(secondPacket[33]).toBe(MouseMacroEvent.LEFT_CLICK);
	});
});

describe('CustomMacroBuilder Configuration', () => {
	test('should allow providing custom MacrosBuilder to avoid overwriting other buttons', () => {
		const customMacros = new MacrosBuilder();
		// Change Forward to Middle-Click (index 21)
		customMacros.setMacro(Button.FORWARD, [FirmwareAction.MIDDLE_CLICK, 0x00, 0x00]);

		const builder = new CustomMacroBuilder({
			macrosBuilder: customMacros,
			targetButton: Button.BACKWARD,
		});

		const [macroPacket] = builder.build(ConnectionMode.Adapter);

		// The macroPacket should have the Middle Click for Forward button (index 21)
		expect(macroPacket[21]).toBe(FirmwareAction.MIDDLE_CLICK);
	});

	test('should allow providing MacroBuilderOptions to avoid overwriting other buttons', () => {
		const builder = new CustomMacroBuilder({
			macrosBuilder: {
				forward: [FirmwareAction.DISABLE_BUTTON, 0x00, 0x00],
				backward: [FirmwareAction.BACKWARD, 0x00, 0x00],
			},
			targetButton: Button.FORWARD,
		});

		const [macroPacket] = builder.build(ConnectionMode.Adapter);

		// The forward button is at index 21
		expect(macroPacket[21]).toBe(FirmwareAction.CUSTOM_MACRO);
		expect(macroPacket[24]).toBe(FirmwareAction.BACKWARD);
	});

	test('should allow setting target button with custom MacrosBuilder via method', () => {
		const customMacros = new MacrosBuilder();
		customMacros.setMacro(Button.FORWARD, [FirmwareAction.DISABLE_BUTTON, 0x00, 0x00]);

		const builder = new CustomMacroBuilder();
		builder.setTargetButton(Button.BACKWARD, customMacros);

		const [macroPacket] = builder.build(ConnectionMode.Adapter);

		// Forward should be disabled (0x01)
		expect(macroPacket[21]).toBe(FirmwareAction.DISABLE_BUTTON);
		// Backward should be Custom Macro (0x12)
		expect(macroPacket[24]).toBe(FirmwareAction.CUSTOM_MACRO);
	});

	test('should allow setting target button with MacroBuilderOptions via method', () => {
		const builder = new CustomMacroBuilder();
		builder.setTargetButton(Button.BACKWARD, {
			forward: [FirmwareAction.DISABLE_BUTTON, 0x00, 0x00],
		});

		const [macroPacket] = builder.build(ConnectionMode.Adapter);

		// Forward should be disabled (0x01)
		expect(macroPacket[21]).toBe(FirmwareAction.DISABLE_BUTTON);
		// Backward should be Custom Macro (0x12)
		expect(macroPacket[24]).toBe(FirmwareAction.CUSTOM_MACRO);
	});

	test('should cap the event counter at 47 even if more events are added', () => {
		const builder = new CustomMacroBuilder();
		// Add 50 events
		for (let i = 0; i < 50; i++) {
			builder.addEvent(KeyCode.A);
		}

		const [, secondPacket] = builder.build(ConnectionMode.Adapter);

		// Capacity is 47, so the counter at index 29 should be 47
		expect(secondPacket[29]).toBe(47);
	});
});

describe('CustomMacroBuilder X3Wired forward button macro (live FA61 captures)', () => {
	test('loop 1 press A then release A matches live capture exactly', () => {
		const builder = new CustomMacroBuilder({
			targetButton: Button.FORWARD,
			playOptions: { mode: MacroMode.THE_NUMBER_OF_TIME_TO_PLAY, times: 1 },
		});
		builder.addEvent(KeyCode.A, 10);
		builder.addEvent(KeyCode.A, 10, true);

		const [bindPacket, page0, page1, page2] = builder.build(ConnectionMode.X3Wired);

		// Bind packet (wValue 0x0308) – X3 default with FORWARD = custom macro
		expect(bindPacket.toString('hex')).toBe(
			'083b010200000300000400000d00003c00000f00001200070500003c00' +
				'000100000100000100000100000100000100000100000a000009000000d5',
		);

		// Page 0 (wValue 0x0309) – header 09 40, button 07, page 00,
		//   mode=00, times=01, event count=02, events: press A [01,04] release A [81,04]
		expect(page0.toString('hex')).toBe(
			'0940070000000000010000000000000000000000000000000000000000' +
				'0201048104000000000000000000000000000000000000000000000000000000000000',
		);

		// Page 1 (wValue 0x0309) – all events fit in page 0
		expect(page1.toString('hex')).toBe(
			'0940070100000000000000000000000000000000000000000000000000' +
				'0000000000000000000000000000000000000000000000000000000000000000000000',
		);

		// Page 2 (wValue 0x0309) – header 09 40 (X3 uses 40), checksum 0x008d
		expect(page2.toString('hex')).toBe(
			'09400702000000000000008d0000000000000000000000000000000000' +
				'0000000000000000000000000000000000000000000000000000000000000000000000',
		);
	});

	test('loop 2 page 0 times=2 and page 2 checksum=0x008e match live capture', () => {
		const builder = new CustomMacroBuilder({
			targetButton: Button.FORWARD,
			playOptions: { mode: MacroMode.THE_NUMBER_OF_TIME_TO_PLAY, times: 2 },
		});
		builder.addEvent(KeyCode.A, 10);
		builder.addEvent(KeyCode.A, 10, true);

		const [, page0, , page2] = builder.build(ConnectionMode.X3Wired);

		// Page 0 – offset 8 = 0x02 (loop 2)
		expect(page0.toString('hex')).toBe(
			'0940070000000000020000000000000000000000000000000000000000' +
				'0201048104000000000000000000000000000000000000000000000000000000000000',
		);

		// Page 2 – checksum = 0x008e
		expect(page2.toString('hex')).toBe(
			'09400702000000000000008e0000000000000000000000000000000000' +
				'0000000000000000000000000000000000000000000000000000000000000000000000',
		);
	});

	test('non-X3 mode (Adapter) still uses page 2 header byte 1 = 0x0c', () => {
		const builder = new CustomMacroBuilder({
			targetButton: Button.FORWARD,
			playOptions: { mode: MacroMode.THE_NUMBER_OF_TIME_TO_PLAY, times: 1 },
		});
		builder.addEvent(KeyCode.A, 10);
		builder.addEvent(KeyCode.A, 10, true);

		const [, , , page2] = builder.build(ConnectionMode.Adapter);

		// Page 2 should start with 09 0c ...
		expect(page2.toString('hex').startsWith('090c')).toBe(true);
	});

	test('long capture with 46 events spills to page 1 and page 2 checksum 0x0e06', () => {
		const builder = new CustomMacroBuilder({
			targetButton: Button.FORWARD,
			playOptions: { mode: MacroMode.THE_NUMBER_OF_TIME_TO_PLAY, times: 1 },
		});

		// Page 0 events (17) — release A, press A, release A
		builder.addEvent(KeyCode.A, 10, true);
		builder.addEvent(KeyCode.A, 10, false);
		builder.addEvent(KeyCode.A, 10, true);
		// press S, release S, press S, release S
		builder.addEvent(KeyCode.S, 10, false);
		builder.addEvent(KeyCode.S, 10, true);
		builder.addEvent(KeyCode.S, 10, false);
		builder.addEvent(KeyCode.S, 10, true);
		// press D, release D, press D, release D
		builder.addEvent(KeyCode.D, 10, false);
		builder.addEvent(KeyCode.D, 10, true);
		builder.addEvent(KeyCode.D, 10, false);
		builder.addEvent(KeyCode.D, 10, true);
		// press F, release F, press F, release F, press F, release F
		builder.addEvent(KeyCode.F, 10, false);
		builder.addEvent(KeyCode.F, 10, true);
		builder.addEvent(KeyCode.F, 10, false);
		builder.addEvent(KeyCode.F, 10, true);
		builder.addEvent(KeyCode.F, 10, false);
		builder.addEvent(KeyCode.F, 10, true);

		// Page 1 events (29) — G: 3 pairs
		builder.addEvent(KeyCode.G, 10, false);
		builder.addEvent(KeyCode.G, 10, true);
		builder.addEvent(KeyCode.G, 10, false);
		builder.addEvent(KeyCode.G, 10, true);
		builder.addEvent(KeyCode.G, 10, false);
		builder.addEvent(KeyCode.G, 10, true);
		// H: 2 pairs
		builder.addEvent(KeyCode.H, 10, false);
		builder.addEvent(KeyCode.H, 10, true);
		builder.addEvent(KeyCode.H, 10, false);
		builder.addEvent(KeyCode.H, 10, true);
		// J: 3 pairs
		builder.addEvent(KeyCode.J, 10, false);
		builder.addEvent(KeyCode.J, 10, true);
		builder.addEvent(KeyCode.J, 10, false);
		builder.addEvent(KeyCode.J, 10, true);
		builder.addEvent(KeyCode.J, 10, false);
		builder.addEvent(KeyCode.J, 10, true);
		// K: 3 pairs
		builder.addEvent(KeyCode.K, 10, false);
		builder.addEvent(KeyCode.K, 10, true);
		builder.addEvent(KeyCode.K, 10, false);
		builder.addEvent(KeyCode.K, 10, true);
		builder.addEvent(KeyCode.K, 10, false);
		builder.addEvent(KeyCode.K, 10, true);
		// L: 3 pairs + 1 extra press
		builder.addEvent(KeyCode.L, 10, false);
		builder.addEvent(KeyCode.L, 10, true);
		builder.addEvent(KeyCode.L, 10, false);
		builder.addEvent(KeyCode.L, 10, true);
		builder.addEvent(KeyCode.L, 10, false);
		builder.addEvent(KeyCode.L, 10, true);
		builder.addEvent(KeyCode.L, 10, false);

		const [bindPacket, page0, page1, page2] = builder.build(ConnectionMode.X3Wired);

		// Bind packet (wValue 0x0308) – X3 default with FORWARD = custom macro
		expect(bindPacket.toString('hex')).toBe(
			'083b010200000300000400000d00003c00000f00001200070500003c00000100000100000100000100000100000100000100000a000009000000d5',
		);

		// Page 0 (wValue 0x0309) – header, mode=00, times=01, event count=46 (0x2e), 17 events
		expect(page0.toString('hex')).toBe(
			'09400700000000000100000000000000000000000000000000000000002e81040104810401168116011681160107810701078107010981090109810901098109',
		);

		// Page 1 (wValue 0x0309) – 29 events + trailing zeros
		expect(page1.toString('hex')).toBe(
			'09400701010a810a010a810a010a810a010b810b010b810b010d810d010d810d010d810d010e810e010e810e010e810e010f810f010f810f010f810f010f0000',
		);

		// Page 2 (wValue 0x0309) – header 09 40, checksum 0x0e06
		expect(page2.toString('hex')).toBe(
			'094007020000000000000e0600000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000',
		);
	});
});
