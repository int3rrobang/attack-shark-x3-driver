import { UserPreferencesBuilder } from '../../protocols/UserPreferencesBuilder.js';
import type { ConnectionMode } from '../../types.js';
import { HEX_PREFS_HELP } from '../help.js';
import { parseUserPreferencesOptions } from '../helpers.js';

export const help = HEX_PREFS_HELP;

export function run(mode: ConnectionMode, flags: Record<string, string | boolean>): void {
	const options = parseUserPreferencesOptions(flags);

	const builder = new UserPreferencesBuilder(options);
	const buffer = builder.build(mode);
	console.log(buffer.toString('hex'));
}
