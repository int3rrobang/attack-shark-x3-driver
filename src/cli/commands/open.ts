import { ConnectionMode } from '../../types.js';
import { OPEN_HELP } from '../help.js';
import { withDriver } from '../helpers.js';

export const help = OPEN_HELP;

export async function run(mode: ConnectionMode, delayMs: number): Promise<void> {
	await withDriver(mode, delayMs, async (_driver) => {
		// just open and close
	});
	console.log(`Device opened successfully in ${ConnectionMode[mode]} mode.`);
}
