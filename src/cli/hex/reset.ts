import { InternalStateResetReportBuilder } from '../../protocols/InternalStateResetReportBuilder.js';
import type { TransportKind } from '../../types.js';
import { HEX_RESET_HELP } from '../help.js';

export const help = HEX_RESET_HELP;

export function run(transport: TransportKind): void {
	const buffer = new InternalStateResetReportBuilder().build(transport);
	console.log(buffer.toString('hex'));
}
