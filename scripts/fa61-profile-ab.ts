#!/usr/bin/env bun

import { parseArgs } from 'node:util';
import { createInterface } from 'node:readline/promises';
import {
	buildButtonAction,
	buildProfileSelector,
	buildReadSelector,
	buildSingleStageDpi,
	decodeFirstDpi,
	decodeProfileMetadata,
	FA61_REPORT_LENGTH,
	hasLoadedDistinctProfile,
	Fa61ProfileProbe,
	type Fa61Snapshot,
	type ProfileObservation,
} from '../src/experimental/Fa61ProfileProbe.js';

const HELP = `FA61/M600 profile A/B probe

Usage:
  bun scripts/fa61-profile-ab.ts --dry-run [options]
  bun scripts/fa61-profile-ab.ts --write --out <session.json> [options]

Options:
  --out <path>             JSON session log path; required for hardware runs
  --label <text>           Mouse label stored in the session log
  --delay-ms <ms>          Delay after every HID selector/write (default: 500)
  --poll-count <count>     Maximum profile-load observations (default: 10)
  --primary-dpi <dpi>      Profile 1 fingerprint (default: 800)
  --secondary-dpi <dpi>    Profile 2 fingerprint (default: 3200)
  --path-contains <text>   Select one FA61 Col04 path when multiple are connected
  --with-cycle             After direct switching works, test Forward = 34 00 00 once
  --keep                   Leave test DPI values instead of restoring both profiles
  --write                  Required acknowledgement for configuration writes
  --dry-run                Print packets and workflow without opening hardware
  --help                    Show this help

Safety:
  The runner requires profile 1 at startup, saves a complete baseline before writing,
  waits at least --delay-ms after every HID transaction, and will not write profile 2
  unless report 0x04 proves that profile 1's distinctive live image disappeared.
`;

interface SessionEvent {
	readonly at: string;
	readonly step: string;
	readonly data: unknown;
}

interface SessionLog {
	readonly startedAt: string;
	readonly label: string;
	readonly settings: {
		readonly delayMs: number;
		readonly pollCount: number;
		readonly primaryDpi: number;
		readonly secondaryDpi: number;
		readonly withCycle: boolean;
		readonly keep: boolean;
	};
	baseline?: Fa61Snapshot;
	secondaryBaseline?: Fa61Snapshot;
	outcome?: string;
	readonly events: SessionEvent[];
}

const parsed = parseArgs({
	args: process.argv.slice(2),
	options: {
		out: { type: 'string' },
		label: { type: 'string', default: 'FA61 mouse' },
		'delay-ms': { type: 'string', default: '500' },
		'poll-count': { type: 'string', default: '10' },
		'primary-dpi': { type: 'string', default: '800' },
		'secondary-dpi': { type: 'string', default: '3200' },
		'path-contains': { type: 'string' },
		'with-cycle': { type: 'boolean', default: false },
		keep: { type: 'boolean', default: false },
		write: { type: 'boolean', default: false },
		'dry-run': { type: 'boolean', default: false },
		help: { type: 'boolean', short: 'h', default: false },
	},
	strict: true,
	allowPositionals: false,
});

const values = parsed.values;
if (values.help) {
	console.log(HELP);
	process.exit(0);
}

function parseInteger(value: string, name: string, minimum: number): number {
	const parsedValue = Number(value);
	if (!Number.isInteger(parsedValue) || parsedValue < minimum) {
		throw new Error(`${name} must be an integer of at least ${minimum}, got ${value}`);
	}
	return parsedValue;
}

const delayMs = parseInteger(values['delay-ms'], '--delay-ms', 100);
const pollCount = parseInteger(values['poll-count'], '--poll-count', 1);
const primaryDpi = parseInteger(values['primary-dpi'], '--primary-dpi', 50);
const secondaryDpi = parseInteger(values['secondary-dpi'], '--secondary-dpi', 50);
const pathContains = values['path-contains'];

if (values['dry-run']) {
	console.log(
		JSON.stringify(
			{
				delayMs,
				pollCount,
				primaryDpi,
				secondaryDpi,
				readSelectors: {
					version: buildReadSelector(0x0b, FA61_REPORT_LENGTH.VERSION).toString('hex'),
					profileMetadata: buildReadSelector(0x0c, FA61_REPORT_LENGTH.PROFILE_METADATA).toString('hex'),
					dpiProfile1: buildReadSelector(0x04, FA61_REPORT_LENGTH.DPI, 1).toString('hex'),
					dpiProfile2: buildReadSelector(0x04, FA61_REPORT_LENGTH.DPI, 2).toString('hex'),
					preferencesProfile1: buildReadSelector(0x05, FA61_REPORT_LENGTH.PREFERENCES, 1).toString('hex'),
					buttonsProfile1: buildReadSelector(0x08, FA61_REPORT_LENGTH.BUTTONS, 1).toString('hex'),
				},
				profileSelectors: {
					profile1: buildProfileSelector(1, 5).toString('hex'),
					profile2: buildProfileSelector(2, 5).toString('hex'),
				},
				guard: 'Profile 2 is never written until its live 0x04 differs from profile 1 fingerprint',
			},
			null,
			2,
		),
	);
	process.exit(0);
}

if (!values.write) throw new Error('Hardware mode requires --write');
if (!values.out) throw new Error('Hardware mode requires --out <session.json>');
if (primaryDpi === secondaryDpi) throw new Error('Primary and secondary DPI fingerprints must differ');
const PERSISTENCE_DELAY_MS = 5000;

const session: SessionLog = {
	startedAt: new Date().toISOString(),
	label: values.label,
	settings: {
		delayMs,
		pollCount,
		primaryDpi,
		secondaryDpi,
		withCycle: values['with-cycle'],
		keep: values.keep,
	},
	events: [],
};
const outputPath = values.out;
const terminal = createInterface({ input: process.stdin, output: process.stdout });
let configurationTouched = false;

function record(step: string, data: unknown): void {
	session.events.push({ at: new Date().toISOString(), step, data });
	console.log(`\n[${step}]`);
	console.log(typeof data === 'string' ? data : JSON.stringify(data, null, 2));
}

async function saveSession(): Promise<void> {
	await Bun.write(outputPath, `${JSON.stringify(session, null, 2)}\n`);
}

async function prompt(message: string): Promise<void> {
	await terminal.question(`${message}\nPress Enter when ready... `);
}

function discover(): Promise<Fa61ProfileProbe> {
	return Fa61ProfileProbe.discover({
		delayMs,
		...(pathContains === undefined ? {} : { pathContains }),
	});
}

async function pollFor(
	probe: Fa61ProfileProbe,
	targetProfile: number,
	predicate: (observation: ProfileObservation) => boolean,
): Promise<{
	readonly matched: boolean;
	readonly observations: readonly ProfileObservation[];
	readonly transientErrors: readonly string[];
}> {
	const observations: ProfileObservation[] = [];
	const transientErrors: string[] = [];
	for (let attempt = 0; attempt < pollCount; attempt++) {
		try {
			const observation = await probe.observeProfile(targetProfile);
			observations.push(observation);
			if (predicate(observation)) return { matched: true, observations, transientErrors };
		} catch (error) {
			transientErrors.push(error instanceof Error ? error.message : String(error));
		}
	}
	return { matched: false, observations, transientErrors };
}

async function requireLoaded(probe: Fa61ProfileProbe, profile: number, dpiHex: string, step: string): Promise<void> {
	const result = await pollFor(
		probe,
		profile,
		(observation) => observation.metadata.current === profile && observation.dpiHex === dpiHex,
	);
	record(step, result);
	await saveSession();
	if (!result.matched) throw new Error(`Profile ${profile} did not load the expected DPI image`);
}

async function writeSnapshotSections(probe: Fa61ProfileProbe, snapshot: Fa61Snapshot): Promise<void> {
	await probe.writeReport(Buffer.from(snapshot.dpi, 'hex'));
	await probe.writeReport(Buffer.from(snapshot.preferences, 'hex'));
	await probe.writeReport(Buffer.from(snapshot.buttons, 'hex'));
}

async function loadProfileForRecovery(probe: Fa61ProfileProbe, profile: number): Promise<void> {
	const temporaryProfile = profile === 1 ? 2 : 1;
	await probe.selectProfile(temporaryProfile, 5);
	await probe.selectProfile(profile, 5);
	await Bun.sleep(PERSISTENCE_DELAY_MS);
}

async function recoverAfterError(): Promise<void> {
	if (!configurationTouched || !session.baseline) return;

	const probe = await discover();
	if (session.secondaryBaseline) {
		await loadProfileForRecovery(probe, 2);
		await writeSnapshotSections(probe, session.secondaryBaseline);
		await Bun.sleep(PERSISTENCE_DELAY_MS);
	}

	const metadata = decodeProfileMetadata(Buffer.from(session.baseline.profileMetadata, 'hex'));
	await loadProfileForRecovery(probe, metadata.current);
	await writeSnapshotSections(probe, session.baseline);
	await Bun.sleep(PERSISTENCE_DELAY_MS);
	await probe.selectProfile(metadata.current, metadata.maximum);
	record('error-recovery', await probe.snapshot());
}

async function restorePrimary(probe: Fa61ProfileProbe, baseline: Fa61Snapshot): Promise<void> {
	await writeSnapshotSections(probe, baseline);
	await Bun.sleep(PERSISTENCE_DELAY_MS);
	const metadata = decodeProfileMetadata(Buffer.from(baseline.profileMetadata, 'hex'));
	await probe.selectProfile(metadata.current, metadata.maximum);
	record('primary-restored', await probe.observeProfile(metadata.current));
	await saveSession();
}

async function main(): Promise<void> {
	let probe = await discover();
	const baseline = await probe.snapshot();
	session.baseline = baseline;
	const baselineMetadata = decodeProfileMetadata(Buffer.from(baseline.profileMetadata, 'hex'));
	record('baseline', {
		...baseline,
		decodedProfile: baselineMetadata,
		firstDpi: decodeFirstDpi(Buffer.from(baseline.dpi, 'hex')),
	});
	await saveSession();
	if (baselineMetadata.current !== 1) {
		throw new Error(`Start the mouse in profile 1; current metadata reports profile ${baselineMetadata.current}`);
	}

	const confirmation = await terminal.question(
		`Type WRITE to seed profile 1 at ${primaryDpi} DPI. The baseline is saved at ${outputPath}: `,
	);
	if (confirmation !== 'WRITE') throw new Error('Write confirmation did not match WRITE');

	configurationTouched = true;
	await probe.selectProfile(1, 5);
	const profile1Metadata = await pollFor(probe, 1, (observation) => observation.metadata.current === 1);
	record('profile-1-metadata', profile1Metadata);
	if (!profile1Metadata.matched) throw new Error('Profile 1 metadata was not accepted');

	const profile1Source = await probe.readReport(0x04, FA61_REPORT_LENGTH.DPI, 1);
	const profile1Packet = buildSingleStageDpi(profile1Source, primaryDpi, 1);
	await probe.writeReport(profile1Packet);
	const profile1Readback = await probe.readReport(0x04, FA61_REPORT_LENGTH.DPI, 1);
	record('profile-1-seeded', {
		expected: profile1Packet.toString('hex'),
		readback: profile1Readback.toString('hex'),
		matches: profile1Readback.equals(profile1Packet),
	});
	await saveSession();
	if (!profile1Readback.equals(profile1Packet))
		throw new Error('Profile 1 live DPI readback did not match the write');

	await prompt('Power-cycle the mouse with profile 1 selected to verify flash-backed reload.');
	probe = await discover();
	await requireLoaded(probe, 1, profile1Packet.toString('hex'), 'profile-1-reboot-verification');

	await probe.selectProfile(2, 5);
	const profile2Load = await pollFor(probe, 2, (observation) =>
		hasLoadedDistinctProfile(observation, 2, profile1Packet.toString('hex')),
	);
	record('profile-2-load-barrier', profile2Load);
	await saveSession();
	if (!profile2Load.matched) {
		session.outcome = 'metadata-only: profile 2 metadata changed without a distinct live DPI image';
		record('stopped-before-profile-2-write', session.outcome);
		await restorePrimary(probe, baseline);
		await saveSession();
		return;
	}

	const finalProfile2Observation = profile2Load.observations.at(-1);
	if (!finalProfile2Observation) throw new Error('Profile 2 load matched without an observation');
	const profile2SourceHex = finalProfile2Observation.dpiHex;
	const profile2Buttons = await probe.readReport(0x08, FA61_REPORT_LENGTH.BUTTONS, 2);
	const profile2Preferences = await probe.readReport(0x05, FA61_REPORT_LENGTH.PREFERENCES, 2);
	const profile2Version = await probe.readReport(0x0b, FA61_REPORT_LENGTH.VERSION);
	const profile2Metadata = await probe.readReport(0x0c, FA61_REPORT_LENGTH.PROFILE_METADATA);
	session.secondaryBaseline = {
		capturedAt: new Date().toISOString(),
		device: probe.device,
		version: profile2Version.toString('hex'),
		profileMetadata: profile2Metadata.toString('hex'),
		dpi: profile2SourceHex,
		preferences: profile2Preferences.toString('hex'),
		buttons: profile2Buttons.toString('hex'),
	};
	await saveSession();

	const profile2Packet = buildSingleStageDpi(Buffer.from(profile2SourceHex, 'hex'), secondaryDpi, 2);
	await probe.writeReport(profile2Packet);
	const profile2Readback = await probe.readReport(0x04, FA61_REPORT_LENGTH.DPI, 2);
	record('profile-2-seeded', {
		expected: profile2Packet.toString('hex'),
		readback: profile2Readback.toString('hex'),
		matches: profile2Readback.equals(profile2Packet),
	});
	await saveSession();
	if (!profile2Readback.equals(profile2Packet))
		throw new Error('Profile 2 live DPI readback did not match the write');

	await probe.selectProfile(1, 5);
	await requireLoaded(probe, 1, profile1Packet.toString('hex'), 'software-switch-to-profile-1');
	await probe.selectProfile(2, 5);
	await requireLoaded(probe, 2, profile2Packet.toString('hex'), 'software-switch-to-profile-2');

	await prompt('Power-cycle the mouse with profile 2 selected to verify its flash-backed reload.');
	probe = await discover();
	await requireLoaded(probe, 2, profile2Packet.toString('hex'), 'profile-2-reboot-verification');

	if (values['with-cycle']) {
		await probe.selectProfile(1, 5);
		await requireLoaded(probe, 1, profile1Packet.toString('hex'), 'cycle-test-profile-1-load');
		const originalButtons = await probe.readReport(0x08, FA61_REPORT_LENGTH.BUTTONS, 1);
		const cycleButtons = buildButtonAction(originalButtons, 21, 0x34, 1);
		await probe.writeReport(cycleButtons);
		const cycleReadback = await probe.readReport(0x08, FA61_REPORT_LENGTH.BUTTONS, 1);
		record('cycle-binding-written', {
			expected: cycleButtons.toString('hex'),
			readback: cycleReadback.toString('hex'),
		});
		await prompt('Press Forward exactly once to test Profile Cycle.');
		const cycleResult = await pollFor(
			probe,
			2,
			(observation) =>
				observation.metadata.current === 2 && observation.dpiHex === profile2Packet.toString('hex'),
		);
		record('physical-cycle-result', cycleResult);
		await probe.selectProfile(1, 5);
		await requireLoaded(probe, 1, profile1Packet.toString('hex'), 'cycle-binding-restore-load');
		await probe.writeReport(originalButtons);
		record('cycle-binding-restored', (await probe.readReport(0x08, FA61_REPORT_LENGTH.BUTTONS, 1)).toString('hex'));
		await saveSession();
	}

	session.outcome = 'profiles 1 and 2 loaded distinct persisted DPI images';
	if (!values.keep) {
		await probe.selectProfile(2, 5);
		await Bun.sleep(PERSISTENCE_DELAY_MS);
		await requireLoaded(probe, 2, profile2Packet.toString('hex'), 'restore-profile-2-load');
		await writeSnapshotSections(probe, session.secondaryBaseline);
		await Bun.sleep(PERSISTENCE_DELAY_MS);
		await probe.selectProfile(1, 5);
		await Bun.sleep(PERSISTENCE_DELAY_MS);
		await requireLoaded(probe, 1, profile1Packet.toString('hex'), 'restore-profile-1-load');
		await restorePrimary(probe, baseline);
	}
	record('complete', session.outcome);
	await saveSession();
}

try {
	await main();
} catch (error) {
	session.outcome = `error: ${error instanceof Error ? error.message : String(error)}`;
	record('error', session.outcome);
	await saveSession().catch(() => undefined);
	try {
		await recoverAfterError();
	} catch (recoveryError) {
		record('error-recovery-failed', recoveryError instanceof Error ? recoveryError.message : String(recoveryError));
	}
	await saveSession().catch(() => undefined);
	process.exitCode = 1;
} finally {
	terminal.close();
}
