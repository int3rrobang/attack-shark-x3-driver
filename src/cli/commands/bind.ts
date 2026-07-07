import { Button, ConnectionMode } from '../../types.js';
import { MacrosBuilder, macroTemplates, MacroName } from '../../protocols/MacrosBuilder.js';
import { BIND_HELP } from '../help.js';
import { withDriver } from '../helpers.js';

const BUTTON_MAP: Record<string, Button> = {
	left: Button.LEFT,
	right: Button.RIGHT,
	middle: Button.MIDDLE,
	forward: Button.FORWARD,
	backward: Button.BACKWARD,
	dpi: Button.DPI,
	'scroll-up': Button.SCROLL_UP,
	'scroll-down': Button.SCROLL_DOWN,
};

export const help = BIND_HELP;

export async function run(
	mode: ConnectionMode,
	delayMs: number,
	flags: Record<string, string | boolean>,
): Promise<void> {
	if (flags['list-actions']) {
		console.log('Available actions (MacroName):');
		for (const name of Object.values(MacroName)) {
			console.log(`  ${name}`);
		}
		return;
	}

	if (flags['list-buttons']) {
		console.log('Available buttons:');
		for (const btn of Object.keys(BUTTON_MAP)) {
			console.log(`  ${btn}`);
		}
		console.log(
			'\nNote: bind sends a full model-default mapping packet; unspecified buttons reset to model defaults, not current device state.',
		);
		console.log(
			'x3-wired/FA61: DPI binds appear ignored; scroll binds are experimental/unsafe and may repeat indefinitely.',
		);
		return;
	}

	const buttonStr = flags['button'];
	const actionStr = flags['action'];

	if (typeof buttonStr !== 'string' || typeof actionStr !== 'string') {
		throw new Error(
			'--button and --action are required. Use --list-buttons and --list-actions to see available values.',
		);
	}

	const button = BUTTON_MAP[buttonStr];
	if (button === undefined) {
		throw new Error(`Unknown button: ${buttonStr}. Use --list-buttons to see available buttons.`);
	}

	const action = actionStr as MacroName;
	const macro = macroTemplates[action];
	if (!macro) {
		throw new Error(`Unknown action: ${actionStr}. Use --list-actions to see available actions.`);
	}

	await withDriver(mode, delayMs, async (driver) => {
		const builder = new MacrosBuilder().setMacro(button, macro);
		await driver.setMacro(builder);
	});

	console.log(`Bound button '${buttonStr}' to action '${actionStr}'.`);
	console.log(
		'Note: bind sends a full model-default mapping packet; unspecified buttons reset to model defaults, not current device state.',
	);
	if (mode === ConnectionMode.X3Wired) {
		console.log(
			'x3-wired/FA61 caveat: DPI binds appear ignored; scroll binds are experimental/unsafe and may repeat indefinitely.',
		);
	}
}
