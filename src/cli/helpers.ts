import { AttackSharkX3, type DpiBuilderOptions, type DpiStages, type StageIndex } from '../index.js';
import type { TransportKind } from '../types.js';
import {
	LightMode,
	type UserPreferencesBuilderOptions,
	type LedSpeed,
	type SleepTime,
	type DeepSleepTime,
	type KeyResponse,
} from '../protocols/UserPreferencesBuilder.js';

export async function withDriver(
	transport: TransportKind,
	delayMs: number,
	fn: (driver: AttackSharkX3) => Promise<void>,
): Promise<void> {
	const driver = new AttackSharkX3({ transport: { kind: transport }, delayMs });
	await driver.open();
	try {
		await fn(driver);
	} finally {
		await driver.close();
	}
}

/**
 * Parse and validate the --stages flag value into positive finite integers.
 * X3 supports one to eight DPI stages on both transports.
 */
export function parseDpiStages(raw: unknown): DpiStages {
	if (typeof raw !== 'string' || raw.trim() === '') {
		throw new Error('--stages is required (comma-separated list of DPI values)');
	}
	if (raw.endsWith(',')) {
		throw new Error('--stages must not have a trailing comma');
	}
	const parts = raw.split(',');
	if (parts.length < 1 || parts.length > 8) {
		throw new Error('--stages must have 1 to 8 comma-separated values');
	}
	const values: number[] = [];
	for (let i = 0; i < parts.length; i++) {
		const s = parts[i]!.trim();
		if (s === '') {
			throw new Error(`--stages entry ${i + 1} is empty`);
		}
		const n = Number(s);
		if (!Number.isFinite(n)) {
			throw new Error(`--stages entry ${i + 1} ("${s}") is not a finite number`);
		}
		if (!Number.isInteger(n)) {
			throw new Error(`--stages entry ${i + 1} (${s}) must be an integer`);
		}
		if (n <= 0) {
			throw new Error(`--stages entry ${i + 1} (${n}) must be a positive integer`);
		}
		values.push(n);
	}
	return values as unknown as DpiStages;
}

/**
 * Parse and validate the --active flag value.
 * Returns undefined when omitted (DpiBuilder defaults to stage 2).
 * Rejects boolean flags, missing values, NaN, non-integer, and out-of-range values.
 */
export function parseActiveStage(raw: unknown, maxStage: StageIndex = 8): StageIndex | undefined {
	if (raw === undefined) {
		return undefined;
	}
	// Reject boolean flags (parser gives `true` for bare --active with no value)
	if (typeof raw === 'boolean') {
		throw new Error(`--active requires a value between 1 and ${maxStage}`);
	}
	const s = String(raw);
	if (s.trim() === '') {
		throw new Error(`--active requires a value between 1 and ${maxStage}`);
	}
	const n = Number(s);
	if (!Number.isFinite(n) || !Number.isInteger(n)) {
		throw new Error(`--active must be an integer between 1 and ${maxStage}, got "${s}"`);
	}
	if (n < 1 || n > maxStage) {
		throw new Error(`--active must be between 1 and ${maxStage}, got ${n}`);
	}
	return n as StageIndex;
}

export function parseDpiSensorOptions(
	flags: Record<string, string | boolean>,
): Pick<DpiBuilderOptions, 'angleSnap' | 'ripplerControl' | 'lod' | 'motionSync'> {
	const options: Pick<DpiBuilderOptions, 'angleSnap' | 'ripplerControl' | 'lod' | 'motionSync'> = {};

	if (flags['angle-snap'] !== undefined) {
		options.angleSnap = parseOnOffFlag(flags['angle-snap'], '--angle-snap');
	}
	if (flags['ripple'] !== undefined) {
		options.ripplerControl = parseOnOffFlag(flags['ripple'], '--ripple');
	}
	if (flags['lod'] !== undefined) {
		options.lod = parseLodFlag(flags['lod']);
	}
	if (flags['motion-sync'] !== undefined) {
		options.motionSync = parseOnOffFlag(flags['motion-sync'], '--motion-sync');
	}

	return options;
}

const USER_PREFS_LIGHT_MAP: Record<string, LightMode> = {
	off: LightMode.Off,
	static: LightMode.Static,
	breathing: LightMode.Breathing,
	neon: LightMode.Neon,
	'color-breathing': LightMode.ColorBreathing,
	'static-dpi': LightMode.StaticDpi,
	'breathing-dpi': LightMode.BreathingDpi,
};

/**
 * Parse and validate user-preference flags into typed {@link UserPreferencesBuilderOptions}.
 * Rejects boolean/bare flags for value flags, and validates numeric ranges.
 */
export function parseUserPreferencesOptions(flags: Record<string, string | boolean>): UserPreferencesBuilderOptions {
	const options: UserPreferencesBuilderOptions = {};

	if (flags['light'] !== undefined) {
		if (typeof flags['light'] !== 'string') {
			throw new Error('--light requires a mode name');
		}
		const lightMode = USER_PREFS_LIGHT_MAP[flags['light']];
		if (lightMode === undefined) {
			throw new Error(
				`Unknown light mode: ${flags['light']}. Supported: off, static, breathing, neon, color-breathing, static-dpi, breathing-dpi`,
			);
		}
		options.lightMode = lightMode;
	}

	if (flags['speed'] !== undefined) {
		if (typeof flags['speed'] !== 'string') {
			throw new Error('--speed requires a numeric value (1-5)');
		}
		const n = Number(flags['speed']);
		if (!Number.isFinite(n) || !Number.isInteger(n)) {
			throw new Error(`--speed must be an integer between 1 and 5, got "${flags['speed']}"`);
		}
		if (n < 1 || n > 5) {
			throw new Error(`--speed must be between 1 and 5, got ${n}`);
		}
		options.ledSpeed = n as LedSpeed;
	}

	if (flags['sleep'] !== undefined) {
		if (typeof flags['sleep'] !== 'string') {
			throw new Error('--sleep requires a numeric value (0.5-30)');
		}
		const n = Number(flags['sleep']);
		if (!Number.isFinite(n)) {
			throw new Error(`--sleep must be a number between 0.5 and 30, got "${flags['sleep']}"`);
		}
		// must use 0.5 steps (validate before range so out-of-range steps get the step message)
		if ((n * 2) % 1 !== 0) {
			throw new Error(`--sleep must use 0.5 steps, got ${n}`);
		}
		if (n < 0.5 || n > 30) {
			throw new Error(`--sleep must be between 0.5 and 30, got ${n}`);
		}
		options.sleepTime = n as SleepTime;
	}

	if (flags['deep-sleep'] !== undefined) {
		if (typeof flags['deep-sleep'] !== 'string') {
			throw new Error('--deep-sleep requires a numeric value (1-60)');
		}
		const n = Number(flags['deep-sleep']);
		if (!Number.isFinite(n) || !Number.isInteger(n)) {
			throw new Error(`--deep-sleep must be an integer between 1 and 60, got "${flags['deep-sleep']}"`);
		}
		if (n < 1 || n > 60) {
			throw new Error(`--deep-sleep must be between 1 and 60, got ${n}`);
		}
		options.deepSleepTime = n as DeepSleepTime;
	}

	if (flags['key-response'] !== undefined) {
		if (typeof flags['key-response'] !== 'string') {
			throw new Error('--key-response requires a numeric value (4-50, even)');
		}
		const n = Number(flags['key-response']);
		if (!Number.isFinite(n) || !Number.isInteger(n)) {
			throw new Error(`--key-response must be an even integer between 4 and 50, got "${flags['key-response']}"`);
		}
		if (n < 4 || n > 50 || n % 2 !== 0) {
			throw new Error(`--key-response must be an even number between 4 and 50, got ${n}`);
		}
		options.keyResponse = n as KeyResponse;
	}

	if (flags['rgb'] !== undefined) {
		if (typeof flags['rgb'] !== 'string') {
			throw new Error('--rgb requires r,g,b (three comma-separated values)');
		}
		const parts = flags['rgb'].split(',');
		if (parts.length !== 3) {
			throw new Error('--rgb must be r,g,b (three comma-separated values)');
		}
		const values: number[] = [];
		for (let i = 0; i < parts.length; i++) {
			const s = parts[i]!.trim();
			if (s === '') {
				throw new Error(`--rgb entry ${i + 1} is empty`);
			}
			const n = Number(s);
			if (!Number.isFinite(n) || !Number.isInteger(n)) {
				throw new Error(`--rgb entry ${i + 1} ("${s}") must be an integer 0-255`);
			}
			if (n < 0 || n > 255) {
				throw new Error(`--rgb entry ${i + 1} (${n}) must be between 0 and 255`);
			}
			values.push(n);
		}
		options.rgb = { r: values[0]!, g: values[1]!, b: values[2]! };
	}

	return options;
}

function parseOnOffFlag(raw: unknown, name: string): boolean {
	if (typeof raw !== 'string') {
		throw new Error(`${name} requires on or off`);
	}
	if (raw === 'on') return true;
	if (raw === 'off') return false;
	throw new Error(`${name} must be on or off, got "${raw}"`);
}

function parseLodFlag(raw: unknown): 1 | 2 {
	if (typeof raw !== 'string') {
		throw new Error('--lod requires 1 or 2');
	}
	if (raw === '1') return 1;
	if (raw === '2') return 2;
	throw new Error(`--lod must be 1 or 2, got "${raw}"`);
}
