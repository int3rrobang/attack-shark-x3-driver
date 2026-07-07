import { InternalStateResetReportBuilder } from '../../protocols/InternalStateResetReportBuilder.js';
import type { ConnectionMode } from '../../types.js';
import { HEX_RESET_HELP } from '../help.js';

export const help = HEX_RESET_HELP;

export function run(mode: ConnectionMode): void {
	const buffer = new InternalStateResetReportBuilder().build(mode);
	console.log(buffer.toString('hex'));
}
