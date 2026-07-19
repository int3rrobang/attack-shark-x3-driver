import type { TransportKind } from '../../types.js';
import { BATTERY_HELP } from '../help.js';
import { withDriver } from '../helpers.js';

export const help = BATTERY_HELP;

export async function run(transport: TransportKind, delayMs: number): Promise<void> {
	await withDriver(transport, delayMs, async (driver) => {
		const level = await driver.getBatteryLevel();
		if (level === -1) {
			console.log('Battery: unavailable (wired transport)');
		} else {
			console.log(`Battery: ${level}%`);
		}
	});
}
