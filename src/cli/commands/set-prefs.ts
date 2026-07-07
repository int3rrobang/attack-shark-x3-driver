import type { ConnectionMode } from '../../types.js';
import { SET_PREFS_HELP } from '../help.js';
import { withDriver, parseUserPreferencesOptions } from '../helpers.js';

export const help = SET_PREFS_HELP;

export async function run(
	mode: ConnectionMode,
	delayMs: number,
	flags: Record<string, string | boolean>,
): Promise<void> {
	const options = parseUserPreferencesOptions(flags);

	await withDriver(mode, delayMs, async (driver) => {
		await driver.setUserPreferences(options);
	});
	console.log('Preferences set successfully.');
}
