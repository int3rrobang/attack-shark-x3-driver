import { Rate } from '../../protocols/PollingRateBuilder.js';
import type { TransportKind } from '../../types.js';
import { SET_RATE_HELP } from '../help.js';
import { withDriver } from '../helpers.js';

const RATE_MAP: Record<number, Rate> = {
	125: Rate.powerSaving,
	250: Rate.office,
	500: Rate.gaming,
	1000: Rate.eSports,
};

export const help = SET_RATE_HELP;

/**
 * Parse and validate a raw rate value into a {@link Rate} enum member.
 * Accepts a numeric string; rejects booleans, undefined, and invalid values.
 */
export function parseRate(raw: unknown): Rate {
	if (typeof raw !== 'string') {
		throw new Error('Rate is required (125, 250, 500, or 1000)');
	}
	const rateNum = Number(raw);
	const rate = RATE_MAP[rateNum];
	if (rate === undefined) {
		throw new Error(`Invalid rate: ${raw}. Must be 125, 250, 500, or 1000.`);
	}
	return rate;
}

export async function run(
	transport: TransportKind,
	delayMs: number,
	flags: Record<string, string | boolean>,
	positionals: string[],
): Promise<void> {
	// --rate flag takes priority
	const rateStr = flags['rate'];
	if (typeof rateStr === 'string') {
		const rate = parseRate(rateStr);
		await withDriver(transport, delayMs, async (driver) => {
			await driver.setPollingRate(rate);
		});
		console.log(`Polling rate set to ${rateStr} Hz.`);
		return;
	}

	// Fallback: first positional arg as shorthand (e.g. "set-rate 500")
	const posRate = positionals[0];
	if (posRate !== undefined) {
		const rate = parseRate(posRate);
		await withDriver(transport, delayMs, async (driver) => {
			await driver.setPollingRate(rate);
		});
		console.log(`Polling rate set to ${posRate} Hz.`);
		return;
	}

	throw new Error('Rate is required (125, 250, 500, or 1000). Use --rate or a positional argument.');
}
