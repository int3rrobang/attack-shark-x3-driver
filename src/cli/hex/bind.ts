import { Button, type TransportKind } from '../../types.js';
import { MacrosBuilder, macroTemplates, type MacroName } from '../../protocols/MacrosBuilder.js';
import { HEX_BIND_HELP } from '../help.js';

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

export const help = HEX_BIND_HELP;

export function run(transport: TransportKind, flags: Record<string, string | boolean>): void {
	const buttonStr = flags['button'];
	const actionStr = flags['action'];

	if (typeof buttonStr !== 'string' || typeof actionStr !== 'string') {
		throw new Error('--button and --action are required.');
	}

	const button = BUTTON_MAP[buttonStr];
	if (button === undefined) {
		throw new Error(`Unknown button: ${buttonStr}`);
	}

	const action = actionStr as MacroName;
	const macro = macroTemplates[action];
	if (!macro) {
		throw new Error(`Unknown action: ${actionStr}`);
	}

	const builder = new MacrosBuilder().setMacro(button, macro);
	builder.build(transport);
	console.log(builder.toString());
}
