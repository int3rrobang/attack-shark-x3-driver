import { PollingRateBuilder, Rate } from '../../protocols/PollingRateBuilder.js';
import type { TransportKind } from '../../types.js';
import { HEX_RATE_HELP } from '../help.js';

const RATE_MAP: Record<number, Rate> = {
	125: Rate.powerSaving,
	250: Rate.office,
	500: Rate.gaming,
	1000: Rate.eSports,
};

export const help = HEX_RATE_HELP;

export function run(transport: TransportKind, flags: Record<string, string | boolean>): void {
	const rateStr = flags['rate'];
	if (typeof rateStr !== 'string') {
		throw new Error('--rate is required (125, 250, 500, or 1000)');
	}
	const rateNum = Number(rateStr);
	const rate = RATE_MAP[rateNum];
	if (rate === undefined) {
		throw new Error(`Invalid rate: ${rateStr}. Must be 125, 250, 500, or 1000.`);
	}

	const builder = new PollingRateBuilder().setRate(rate);
	builder.build(transport);
	console.log(builder.toString());
}
