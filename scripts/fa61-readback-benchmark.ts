#!/usr/bin/env bun

import { parseArgs } from 'node:util';
import * as HID from 'node-hid';
import {
	buildBenchmarkSelector,
	READBACK_REPORTS,
	type ReadbackReportName,
	summarizeMicroseconds,
	validateReadback,
} from '../src/experimental/Fa61ReadbackBenchmark.js';

const VID = 0x1d57;
const PID = 0xfa61;
const COL04 = /col04/i;
const REPORT_NAMES = ['version', 'profileMetadata', 'dpi', 'buttons'] as const;
const HELP = `FA61 readback latency benchmark

Usage:
  bun scripts/fa61-readback-benchmark.ts --out <result.json> --label <mouse> [options]

Options:
  --out <path>             Required JSON output path
  --label <text>           Device label (default: FA61 mouse)
  --path-contains <text>   Select one Col04 path
  --iterations <count>     Measured transactions per report and mode (default: 50)
  --warmup <count>         Warm-up transactions per report and mode (default: 10)
  --poll-ms <ms>           Delay between not-ready A0 polls (default: 1)
  --cooldown-ms <ms>       Delay after every transaction (default: 20)
  --max-retries <count>    Rearms after malformed responses (default: 3)
  --help                    Show this help

The benchmark is read-only but targeted 0x04/0x08 reads use profile 1. Start in profile 1.
It runs persistent-handle and open-per-transaction modes and validates every response.
`;

const parsed = parseArgs({
	args: process.argv.slice(2),
	options: {
		out: { type: 'string' },
		label: { type: 'string', default: 'FA61 mouse' },
		'path-contains': { type: 'string' },
		iterations: { type: 'string', default: '50' },
		warmup: { type: 'string', default: '10' },
		'poll-ms': { type: 'string', default: '1' },
		'cooldown-ms': { type: 'string', default: '20' },
		'max-retries': { type: 'string', default: '3' },
		help: { type: 'boolean', default: false },
	},
	strict: true,
});

if (parsed.values.help) {
	console.log(HELP);
	process.exit(0);
}
if (!parsed.values.out) throw new Error('--out <path> is required');

function parseCount(value: string, name: string, minimum: number): number {
	const parsedValue = Number(value);
	if (!Number.isInteger(parsedValue) || parsedValue < minimum)
		throw new Error(`--${name} must be an integer of at least ${minimum}, got ${value}`);
	return parsedValue;
}

const iterations = parseCount(parsed.values.iterations, 'iterations', 1);
const warmup = parseCount(parsed.values.warmup, 'warmup', 0);
const pollMs = parseCount(parsed.values['poll-ms'], 'poll-ms', 0);
const cooldownMs = parseCount(parsed.values['cooldown-ms'], 'cooldown-ms', 10);
const maxRetries = parseCount(parsed.values['max-retries'], 'max-retries', 0);
const pathFilter = parsed.values['path-contains']?.toLowerCase();
const candidates = (await HID.devicesAsync()).filter(
	(device) =>
		device.vendorId === VID &&
		device.productId === PID &&
		COL04.test(device.path ?? '') &&
		(pathFilter === undefined || (device.path ?? '').toLowerCase().includes(pathFilter)),
);
if (candidates.length !== 1)
	throw new Error(
		candidates.length === 0
			? 'No FA61 Col04 device found'
			: `Found ${candidates.length} FA61 Col04 devices; use --path-contains`,
	);
const candidate = candidates[0];
if (!candidate?.path) throw new Error('Selected FA61 Col04 device has no path');
const devicePath = candidate.path;

interface Attempt {
	readonly selectorUs: number;
	readonly readyUs: number;
	readonly getUs: number;
	readonly validationUs: number;
	readonly totalUs: number;
	readonly polls: number;
	readonly beforeStatus: string;
	readonly readyStatus: string;
	readonly reportHex: string;
	readonly valid: boolean;
	readonly validationErrors: readonly string[];
}

interface Transaction {
	readonly openUs: number;
	readonly valid: boolean;
	readonly retries: number;
	readonly totalUs: number;
	readonly attempts: readonly Attempt[];
}

const now = (): bigint => process.hrtime.bigint();
const elapsedUs = (start: bigint, end: bigint = now()): number => Number((end - start) / 1000n);

async function attemptRead(device: HID.HIDAsync, name: ReadbackReportName): Promise<Attempt> {
	const report = READBACK_REPORTS[name];
	const beforeStatus = Buffer.from(await device.getFeatureReport(0xa0, 8));
	const started = now();
	await device.sendFeatureReport(buildBenchmarkSelector(name, 1));
	const selectorFinished = now();

	let readyStatus = Buffer.alloc(0);
	let polls = 0;
	const readyStarted = now();
	while (elapsedUs(readyStarted) <= 250_000) {
		polls++;
		readyStatus = Buffer.from(await device.getFeatureReport(0xa0, 8));
		if (readyStatus.length === 8 && readyStatus[0] === 0xa0 && readyStatus[1] === 0x01) break;
		if (pollMs > 0) await Bun.sleep(pollMs);
	}
	const readyFinished = now();
	if (readyStatus[1] !== 0x01) throw new Error(`A0 did not become ready: ${readyStatus.toString('hex')}`);

	const getStarted = now();
	const packet = Buffer.from(await device.getFeatureReport(report.id, report.length));
	const getFinished = now();
	const validation = validateReadback(name, packet, 1);
	const validationFinished = now();
	return {
		selectorUs: elapsedUs(started, selectorFinished),
		readyUs: elapsedUs(selectorFinished, readyFinished),
		getUs: elapsedUs(getStarted, getFinished),
		validationUs: elapsedUs(getFinished, validationFinished),
		totalUs: elapsedUs(started, validationFinished),
		polls,
		beforeStatus: beforeStatus.toString('hex'),
		readyStatus: readyStatus.toString('hex'),
		reportHex: packet.toString('hex'),
		valid: validation.valid,
		validationErrors: validation.errors,
	};
}

async function runTransaction(name: ReadbackReportName, persistent?: HID.HIDAsync): Promise<Transaction> {
	const openStarted = now();
	const device = persistent ?? (await HID.HIDAsync.open(devicePath));
	const opened = now();
	const attempts: Attempt[] = [];
	const transactionStarted = now();
	try {
		for (let retry = 0; retry <= maxRetries; retry++) {
			try {
				const attempt = await attemptRead(device, name);
				attempts.push(attempt);
				if (attempt.valid)
					return {
						openUs: persistent ? 0 : elapsedUs(openStarted, opened),
						valid: true,
						retries: retry,
						totalUs: elapsedUs(transactionStarted),
						attempts,
					};
			} catch (error) {
				if (retry === maxRetries) throw error;
			}
		}
		throw new Error('Retry loop ended unexpectedly');
	} finally {
		if (!persistent) await device.close();
		await Bun.sleep(cooldownMs);
	}
}

function summarizeTransactions(transactions: readonly Transaction[]): object {
	const valid = transactions.filter((transaction) => transaction.valid);
	const finalAttempts = valid
		.map((transaction) => transaction.attempts.at(-1))
		.filter((attempt) => attempt !== undefined);
	const malformed = transactions.flatMap((transaction) => transaction.attempts).filter((attempt) => !attempt.valid);
	return {
		count: transactions.length,
		valid: valid.length,
		malformed: malformed.length,
		malformedPercent: Number(
			((malformed.length / Math.max(1, transactions.flatMap((item) => item.attempts).length)) * 100).toFixed(3),
		),
		meanRetries: Number(
			(valid.reduce((sum, transaction) => sum + transaction.retries, 0) / Math.max(1, valid.length)).toFixed(3),
		),
		openUs: summarizeMicroseconds(valid.map((transaction) => transaction.openUs)),
		selectorUs: summarizeMicroseconds(finalAttempts.map((attempt) => attempt.selectorUs)),
		readyUs: summarizeMicroseconds(finalAttempts.map((attempt) => attempt.readyUs)),
		getUs: summarizeMicroseconds(finalAttempts.map((attempt) => attempt.getUs)),
		validTotalUs: summarizeMicroseconds(valid.map((transaction) => transaction.totalUs)),
		malformedResponses: malformed,
	};
}

const result: Record<string, unknown> = {
	startedAt: new Date().toISOString(),
	label: parsed.values.label,
	device: candidate,
	settings: { iterations, warmup, pollMs, cooldownMs, maxRetries },
	modes: {},
};
const modes = result.modes as Record<string, unknown>;

for (const mode of ['persistent', 'openPerTransaction'] as const) {
	const reportResults: Record<string, unknown> = {};
	modes[mode] = reportResults;
	for (const name of REPORT_NAMES) {
		console.log(`${mode}: ${name}`);
		const persistent = mode === 'persistent' ? await HID.HIDAsync.open(devicePath) : undefined;
		const transactions: Transaction[] = [];
		try {
			for (let index = 0; index < warmup + iterations; index++) {
				const transaction = await runTransaction(name, persistent);
				if (index >= warmup) transactions.push(transaction);
			}
		} finally {
			if (persistent) await persistent.close();
		}
		reportResults[name] = {
			summary: summarizeTransactions(transactions),
			transactions,
		};
		await Bun.write(parsed.values.out, `${JSON.stringify(result, null, 2)}\n`);
		await Bun.sleep(250);
	}
}

result.completedAt = new Date().toISOString();
await Bun.write(parsed.values.out, `${JSON.stringify(result, null, 2)}\n`);
console.log(JSON.stringify(result, null, 2));
