import type { ConnectionMode } from '../../types.js';
import { BATTERY_HELP } from '../help.js';
import { withDriver } from '../helpers.js';

export const help = BATTERY_HELP;

export async function run(mode: ConnectionMode, delayMs: number): Promise<void> {
	await withDriver(mode, delayMs, async (driver) => {
		const level = await driver.getBatteryLevel();
		if (level === -1) {
			console.log('Battery: unavailable (wired mode)');
		} else {
			console.log(`Battery: ${level}%`);
		}
	});
}
