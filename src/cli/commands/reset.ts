import type { TransportKind } from '../../types.js';
import { RESET_HELP } from '../help.js';
import { withDriver } from '../helpers.js';

export const help = RESET_HELP;

export async function run(transport: TransportKind, delayMs: number): Promise<void> {
	await withDriver(transport, delayMs, async (driver) => {
		await driver.reset();
	});
	console.log('Device reset to factory defaults.');
}
