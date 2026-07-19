import type { TransportKind } from '../../types.js';
import { OPEN_HELP } from '../help.js';
import { withDriver } from '../helpers.js';

export const help = OPEN_HELP;

export async function run(transport: TransportKind, delayMs: number): Promise<void> {
	await withDriver(transport, delayMs, async (_driver) => {
		// just open and close
	});
	console.log(`Device opened successfully over ${transport} transport.`);
}
