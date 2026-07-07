import type { ConnectionMode } from '../../types.js';
import { RESET_HELP } from '../help.js';
import { withDriver } from '../helpers.js';

export const help = RESET_HELP;

export async function run(mode: ConnectionMode, delayMs: number): Promise<void> {
	await withDriver(mode, delayMs, async (driver) => {
		await driver.reset();
	});
	console.log('Device reset to factory defaults.');
}
