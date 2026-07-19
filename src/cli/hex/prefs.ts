import { UserPreferencesBuilder } from '../../protocols/UserPreferencesBuilder.js';
import type { TransportKind } from '../../types.js';
import { HEX_PREFS_HELP } from '../help.js';
import { parseUserPreferencesOptions } from '../helpers.js';

export const help = HEX_PREFS_HELP;

export function run(transport: TransportKind, flags: Record<string, string | boolean>): void {
	const options = parseUserPreferencesOptions(flags);

	const builder = new UserPreferencesBuilder(options);
	const buffer = builder.build(transport);
	console.log(buffer.toString('hex'));
}
