import { describe, expect, it } from 'bun:test';
import { parseArgs } from '../src/cli/args.js';
import { parseMode } from '../src/cli/types.js';
import {
	parseActiveStage,
	parseDpiSensorOptions,
	parseDpiStages,
	parseUserPreferencesOptions,
} from '../src/cli/helpers.js';
import { parseRate } from '../src/cli/commands/set-rate.js';
import { ConnectionMode, DpiBuilder, ParamsError, PollingRateBuilder, Rate } from '../src/index.js';
import { LightMode } from '../src/protocols/UserPreferencesBuilder.js';
import { InternalStateResetReportBuilder } from '../src/protocols/InternalStateResetReportBuilder.js';

describe('CLI args parser', () => {
	it('defaults mode to x3-wired when no --mode flag given', () => {
		expect(parseMode(undefined)).toBe(ConnectionMode.X3Wired);
	});

	it('parses --mode x3-wired', () => {
		expect(parseMode('x3-wired')).toBe(ConnectionMode.X3Wired);
	});

	it('parses --mode x3 as alias for x3-wired', () => {
		expect(parseMode('x3')).toBe(ConnectionMode.X3Wired);
	});

	it('parses --mode wired', () => {
		expect(parseMode('wired')).toBe(ConnectionMode.Wired);
	});

	it('parses --mode adapter', () => {
		expect(parseMode('adapter')).toBe(ConnectionMode.Adapter);
	});

	it('throws on unknown mode', () => {
		expect(() => parseMode('bluetooth')).toThrow('Unknown mode');
	});

	it('extracts command from argv', () => {
		const result = parseArgs(['list']);
		expect(result.command).toBe('list');
		expect(result.positionals).toEqual([]);
	});

	it('collects flags before command', () => {
		const result = parseArgs(['--mode', 'wired', 'set-dpi', '--stages', '800,1600,2400,3200,5000,26000']);
		expect(result.command).toBe('set-dpi');
		expect(result.flags.mode).toBe('wired');
		expect(result.flags.stages).toBe('800,1600,2400,3200,5000,26000');
	});

	it('collects flags after command', () => {
		const result = parseArgs(['set-rate', '--rate', '1000']);
		expect(result.command).toBe('set-rate');
		expect(result.flags.rate).toBe('1000');
	});

	it('parses --help as boolean flag', () => {
		const result = parseArgs(['--help']);
		expect(result.flags.help).toBe(true);
		expect(result.command).toBeNull();
	});

	it('parses -h as help boolean flag', () => {
		const result = parseArgs(['-h']);
		expect(result.flags.help).toBe(true);
	});

	it('parses --delay-ms as numeric string', () => {
		const result = parseArgs(['--delay-ms', '300', 'list']);
		expect(result.flags['delay-ms']).toBe('300');
	});

	it('parses hex subcommand with positionals and flags', () => {
		const result = parseArgs(['hex', 'dpi', '--stages', '800,1600,2400,3200,5000,26000']);
		expect(result.command).toBe('hex');
		expect(result.positionals).toEqual(['dpi']);
		expect(result.flags.stages).toBe('800,1600,2400,3200,5000,26000');
	});

	it('parses bind --list-actions as boolean flag', () => {
		const result = parseArgs(['bind', '--list-actions']);
		expect(result.command).toBe('bind');
		expect(result.flags['list-actions']).toBe(true);
	});

	it('parses set-prefs with multiple flags', () => {
		const result = parseArgs([
			'set-prefs',
			'--light',
			'breathing',
			'--speed',
			'5',
			'--rgb',
			'255,0,0',
			'--sleep',
			'10',
			'--deep-sleep',
			'20',
			'--key-response',
			'8',
		]);
		expect(result.command).toBe('set-prefs');
		expect(result.flags.light).toBe('breathing');
		expect(result.flags.speed).toBe('5');
		expect(result.flags.rgb).toBe('255,0,0');
		expect(result.flags.sleep).toBe('10');
		expect(result.flags['deep-sleep']).toBe('20');
		expect(result.flags['key-response']).toBe('8');
	});

	it('treats unknown --flags as boolean when next arg is also a flag', () => {
		const result = parseArgs(['bind', '--list-actions', '--help']);
		expect(result.flags['list-actions']).toBe(true);
		expect(result.flags.help).toBe(true);
	});

	it('last --mode wins when overridden', () => {
		const result = parseArgs(['--mode', 'adapter', 'hex', 'dpi', '--mode', 'x3-wired']);
		expect(result.flags.mode).toBe('x3-wired');
	});
});

describe('CLI DPI validation helpers', () => {
	// --- parseDpiStages ---
	it('parses valid stages string', () => {
		const result = parseDpiStages('800,1600,2400,3200,5000,26000');
		expect(result).toEqual([800, 1600, 2400, 3200, 5000, 26000]);
	});

	it('parses stages with spaces', () => {
		const result = parseDpiStages('800, 1600, 2400, 3200, 5000, 26000');
		expect(result).toEqual([800, 1600, 2400, 3200, 5000, 26000]);
	});

	it('parses X3 variable-length stages', () => {
		expect(parseDpiStages('400,800,1600', ConnectionMode.X3Wired)).toEqual([400, 800, 1600]);
		expect(parseDpiStages('400,800,1600,2400,3200,5000,20400,26000', ConnectionMode.X3Wired)).toEqual([
			400, 800, 1600, 2400, 3200, 5000, 20400, 26000,
		]);
	});

	it('rejects trailing comma', () => {
		expect(() => parseDpiStages('800,1600,2400,3200,5000,26000,')).toThrow(
			'--stages must not have a trailing comma',
		);
	});

	it('rejects negative value', () => {
		expect(() => parseDpiStages('800,-100,2400,3200,5000,26000')).toThrow('must be a positive integer');
	});

	it('rejects zero value', () => {
		expect(() => parseDpiStages('800,0,2400,3200,5000,26000')).toThrow('must be a positive integer');
	});

	it('rejects non-numeric value', () => {
		expect(() => parseDpiStages('800,abc,2400,3200,5000,26000')).toThrow('is not a finite number');
	});

	it('rejects NaN entry', () => {
		expect(() => parseDpiStages('800,NaN,2400,3200,5000,26000')).toThrow('is not a finite number');
	});

	it('rejects empty entry (double comma)', () => {
		expect(() => parseDpiStages('800,,2400,3200,5000,26000')).toThrow('entry 2 is empty');
	});

	it('rejects too few values', () => {
		expect(() => parseDpiStages('800,1600,2400,3200,5000')).toThrow('exactly 6');
	});

	it('rejects too many values', () => {
		expect(() => parseDpiStages('800,1600,2400,3200,5000,26000,999')).toThrow('exactly 6');
	});

	it('rejects X3 stages above 8 values', () => {
		expect(() => parseDpiStages('1,2,3,4,5,6,7,8,9', ConnectionMode.X3Wired)).toThrow('1 to 8');
	});

	it('rejects non-X3 variable-length stages', () => {
		expect(() => parseDpiStages('400,800,1600', ConnectionMode.Wired)).toThrow('exactly 6');
	});

	it('rejects fractional value', () => {
		expect(() => parseDpiStages('800,1600.5,2400,3200,5000,26000')).toThrow('must be an integer');
	});

	it('rejects non-string input', () => {
		expect(() => parseDpiStages(123)).toThrow('--stages is required');
	});

	it('rejects boolean input', () => {
		expect(() => parseDpiStages(true)).toThrow('--stages is required');
	});

	it('rejects empty string', () => {
		expect(() => parseDpiStages('')).toThrow('--stages is required');
	});

	// --- parseActiveStage ---
	it('returns undefined when active is omitted', () => {
		expect(parseActiveStage(undefined)).toBeUndefined();
	});

	it('parses valid active stage 1', () => {
		expect(parseActiveStage('1')).toBe(1);
	});

	it('parses valid active stage 6', () => {
		expect(parseActiveStage('6')).toBe(6);
	});

	it('parses active stage from number-like string', () => {
		expect(parseActiveStage('3')).toBe(3);
	});

	it('rejects active stage 0', () => {
		expect(() => parseActiveStage('0')).toThrow('must be between 1 and 6');
	});

	it('rejects active stage 7', () => {
		expect(() => parseActiveStage('7')).toThrow('must be between 1 and 6');
	});

	it('validates active stage against X3 provided stage count', () => {
		expect(parseActiveStage('3', 3)).toBe(3);
		expect(() => parseActiveStage('4', 3)).toThrow('must be between 1 and 3');
		expect(parseActiveStage('8', 8)).toBe(8);
	});

	it('rejects boolean true for --active (bare flag)', () => {
		expect(() => parseActiveStage(true)).toThrow('--active requires a value');
	});

	it('rejects boolean false for --active', () => {
		expect(() => parseActiveStage(false)).toThrow('--active requires a value');
	});

	it('rejects NaN for active', () => {
		expect(() => parseActiveStage('NaN')).toThrow('must be an integer between 1 and 6');
	});

	it('rejects non-integer for active', () => {
		expect(() => parseActiveStage('2.5')).toThrow('must be an integer between 1 and 6');
	});

	it('rejects empty string for active', () => {
		expect(() => parseActiveStage('')).toThrow('--active requires a value');
	});

	it('parses X3 DPI sensor flags', () => {
		expect(
			parseDpiSensorOptions(
				{ lod: '2', ripple: 'on', 'angle-snap': 'on', 'motion-sync': 'off' },
				ConnectionMode.X3Wired,
			),
		).toEqual({ lod: 2, ripplerControl: true, angleSnap: true, motionSync: false });
	});

	it('rejects X3-only sensor flags in non-X3 mode', () => {
		expect(() => parseDpiSensorOptions({ lod: '2' }, ConnectionMode.Wired)).toThrow('only supported in x3-wired');
		expect(() => parseDpiSensorOptions({ 'motion-sync': 'on' }, ConnectionMode.Adapter)).toThrow(
			'only supported in x3-wired',
		);
	});

	it('allows angle snap and ripple flags in non-X3 modes', () => {
		expect(parseDpiSensorOptions({ ripple: 'off', 'angle-snap': 'on' }, ConnectionMode.Wired)).toEqual({
			ripplerControl: false,
			angleSnap: true,
		});
	});
});

describe('Hex commands (no hardware)', () => {
	it('hex dpi: X3Wired allows DPI up to 26000', () => {
		const builder = new DpiBuilder({ dpiValues: [800, 1600, 2400, 3200, 5000, 26000] });
		const buffer = builder.build(ConnectionMode.X3Wired);
		expect(buffer.length).toBe(52);
		expect(buffer.toString('hex').length).toBe(104);
	});

	it('hex dpi: Wired mode rejects DPI 26000', () => {
		const builder = new DpiBuilder({ dpiValues: [800, 1600, 2400, 3200, 5000, 26000] });
		expect(() => builder.build(ConnectionMode.Wired)).toThrow(ParamsError);
	});

	it('hex dpi: Adapter also rejects DPI 26000', () => {
		const builder = new DpiBuilder({ dpiValues: [800, 1600, 2400, 3200, 5000, 26000] });
		expect(() => builder.build(ConnectionMode.Adapter)).toThrow(ParamsError);
	});

	it('hex dpi: produces different hex for X3Wired vs Wired with same stages', () => {
		const stages: [number, number, number, number, number, number] = [800, 1600, 2400, 3200, 5000, 22000];
		const x3 = new DpiBuilder({ dpiValues: stages });
		const x3Buffer = x3.build(ConnectionMode.X3Wired);
		const wired = new DpiBuilder({ dpiValues: stages });
		const wiredBuffer = wired.build(ConnectionMode.Wired);
		expect(x3Buffer.toString('hex')).not.toBe(wiredBuffer.toString('hex'));
	});

	it('hex dpi: Wired mode returns 104 hex chars (52 bytes)', () => {
		const stages: [number, number, number, number, number, number] = [800, 1600, 2400, 3200, 5000, 22000];
		const builder = new DpiBuilder({ dpiValues: stages });
		const buffer = builder.build(ConnectionMode.Wired);
		expect(buffer.toString('hex').length).toBe(104);
	});

	it('hex dpi: Adapter mode returns 112 hex chars (56 bytes)', () => {
		const stages: [number, number, number, number, number, number] = [800, 1600, 2400, 3200, 5000, 22000];
		const builder = new DpiBuilder({ dpiValues: stages });
		const buffer = builder.build(ConnectionMode.Adapter);
		expect(buffer.toString('hex').length).toBe(112);
	});

	it('hex rate: builds valid hex for all rate values', () => {
		for (const rate of [Rate.powerSaving, Rate.office, Rate.gaming, Rate.eSports]) {
			const builder = new PollingRateBuilder().setRate(rate);
			builder.build(ConnectionMode.Wired);
			expect(builder.toString().length).toBe(18);
		}
	});

	it('hex reset: X3Wired returns 12 hex chars (6 bytes)', () => {
		const buffer = new InternalStateResetReportBuilder().build(ConnectionMode.X3Wired);
		expect(buffer.toString('hex')).toBe('0c0a01fe01fe');
		expect(buffer.toString('hex').length).toBe(12);
	});

	it('hex reset: Wired returns 12 hex chars (6 bytes)', () => {
		const buffer = new InternalStateResetReportBuilder().build(ConnectionMode.Wired);
		expect(buffer.toString('hex')).toBe('0c0a01fe01fe');
		expect(buffer.toString('hex').length).toBe(12);
	});

	it('hex reset: Adapter returns 20 hex chars (10 bytes)', () => {
		const buffer = new InternalStateResetReportBuilder().build(ConnectionMode.Adapter);
		expect(buffer.toString('hex')).toBe('0c0a01fe01fe00000000');
		expect(buffer.toString('hex').length).toBe(20);
	});
});

describe('parseRate', () => {
	it('parses 125 to powerSaving', () => {
		expect(parseRate('125')).toBe(Rate.powerSaving);
	});

	it('parses 250 to office', () => {
		expect(parseRate('250')).toBe(Rate.office);
	});

	it('parses 500 to gaming', () => {
		expect(parseRate('500')).toBe(Rate.gaming);
	});

	it('parses 1000 to eSports', () => {
		expect(parseRate('1000')).toBe(Rate.eSports);
	});

	it('rejects invalid rate string', () => {
		expect(() => parseRate('999')).toThrow('Invalid rate');
	});

	it('rejects boolean true', () => {
		expect(() => parseRate(true)).toThrow('Rate is required');
	});

	it('rejects boolean false', () => {
		expect(() => parseRate(false)).toThrow('Rate is required');
	});

	it('rejects undefined', () => {
		expect(() => parseRate(undefined)).toThrow('Rate is required');
	});

	it('rejects empty string', () => {
		expect(() => parseRate('')).toThrow('Invalid rate');
	});

	it('rejects non-numeric string', () => {
		expect(() => parseRate('fast')).toThrow('Invalid rate');
	});
});

describe('parseUserPreferencesOptions', () => {
	it('parses valid full prefs into expected object', () => {
		const result = parseUserPreferencesOptions({
			light: 'breathing',
			speed: '4',
			rgb: '255,128,0',
			sleep: '15',
			'deep-sleep': '30',
			'key-response': '16',
		});
		expect(result).toEqual({
			lightMode: LightMode.Breathing,
			ledSpeed: 4,
			rgb: { r: 255, g: 128, b: 0 },
			sleepTime: 15,
			deepSleepTime: 30,
			keyResponse: 16,
		});
	});

	it('rejects bare boolean --speed', () => {
		expect(() => parseUserPreferencesOptions({ speed: true })).toThrow('--speed requires a numeric value');
	});

	it('rejects --speed 0', () => {
		expect(() => parseUserPreferencesOptions({ speed: '0' })).toThrow('--speed must be between 1 and 5');
	});

	it('rejects --speed 6', () => {
		expect(() => parseUserPreferencesOptions({ speed: '6' })).toThrow('--speed must be between 1 and 5');
	});

	it('rejects --speed NaN', () => {
		expect(() => parseUserPreferencesOptions({ speed: 'NaN' })).toThrow('must be an integer between 1 and 5');
	});

	it('rejects --speed non-integer', () => {
		expect(() => parseUserPreferencesOptions({ speed: '2.5' })).toThrow('must be an integer between 1 and 5');
	});

	it('rejects --sleep foo', () => {
		expect(() => parseUserPreferencesOptions({ sleep: 'foo' })).toThrow('must be a number between 0.5 and 30');
	});

	it('rejects --sleep NaN', () => {
		expect(() => parseUserPreferencesOptions({ sleep: 'NaN' })).toThrow('must be a number between 0.5 and 30');
	});

	it('rejects --sleep 0', () => {
		expect(() => parseUserPreferencesOptions({ sleep: '0' })).toThrow('--sleep must be between 0.5 and 30');
	});

	it('rejects --sleep 31', () => {
		expect(() => parseUserPreferencesOptions({ sleep: '31' })).toThrow('--sleep must be between 0.5 and 30');
	});

	it('rejects --sleep 0.3 (not 0.5 step)', () => {
		expect(() => parseUserPreferencesOptions({ sleep: '0.3' })).toThrow('--sleep must use 0.5 steps');
	});

	it('rejects bare boolean --sleep', () => {
		expect(() => parseUserPreferencesOptions({ sleep: true })).toThrow('--sleep requires a numeric value');
	});

	it('rejects --deep-sleep 0', () => {
		expect(() => parseUserPreferencesOptions({ 'deep-sleep': '0' })).toThrow(
			'--deep-sleep must be between 1 and 60',
		);
	});

	it('rejects --deep-sleep 61', () => {
		expect(() => parseUserPreferencesOptions({ 'deep-sleep': '61' })).toThrow(
			'--deep-sleep must be between 1 and 60',
		);
	});

	it('rejects --deep-sleep NaN', () => {
		expect(() => parseUserPreferencesOptions({ 'deep-sleep': 'NaN' })).toThrow(
			'must be an integer between 1 and 60',
		);
	});

	it('rejects bare boolean --deep-sleep', () => {
		expect(() => parseUserPreferencesOptions({ 'deep-sleep': true })).toThrow(
			'--deep-sleep requires a numeric value',
		);
	});

	it('rejects --key-response NaN', () => {
		expect(() => parseUserPreferencesOptions({ 'key-response': 'NaN' })).toThrow(
			'must be an even integer between 4 and 50',
		);
	});

	it('rejects --key-response 3', () => {
		expect(() => parseUserPreferencesOptions({ 'key-response': '3' })).toThrow(
			'--key-response must be an even number between 4 and 50',
		);
	});

	it('rejects --key-response 51', () => {
		expect(() => parseUserPreferencesOptions({ 'key-response': '51' })).toThrow(
			'--key-response must be an even number between 4 and 50',
		);
	});

	it('rejects --key-response odd', () => {
		expect(() => parseUserPreferencesOptions({ 'key-response': '7' })).toThrow(
			'--key-response must be an even number between 4 and 50',
		);
	});

	it('rejects bare boolean --key-response', () => {
		expect(() => parseUserPreferencesOptions({ 'key-response': true })).toThrow(
			'--key-response requires a numeric value',
		);
	});

	it('rejects --rgb 255,,0 (empty entry)', () => {
		expect(() => parseUserPreferencesOptions({ rgb: '255,,0' })).toThrow('--rgb entry 2 is empty');
	});

	it('rejects --rgb 255,0 (only two entries)', () => {
		expect(() => parseUserPreferencesOptions({ rgb: '255,0' })).toThrow('--rgb must be r,g,b');
	});

	it('rejects --rgb 255,0,0,0 (four entries)', () => {
		expect(() => parseUserPreferencesOptions({ rgb: '255,0,0,0' })).toThrow('--rgb must be r,g,b');
	});

	it('rejects --rgb 256,0,0 (value > 255)', () => {
		expect(() => parseUserPreferencesOptions({ rgb: '256,0,0' })).toThrow('must be between 0 and 255');
	});

	it('rejects --rgb -1,0,0 (negative)', () => {
		expect(() => parseUserPreferencesOptions({ rgb: '-1,0,0' })).toThrow('must be between 0 and 255');
	});

	it('rejects --rgb 1.5,0,0 (fractional)', () => {
		expect(() => parseUserPreferencesOptions({ rgb: '1.5,0,0' })).toThrow('must be an integer 0-255');
	});

	it('rejects --rgb foo,0,0 (non-numeric)', () => {
		expect(() => parseUserPreferencesOptions({ rgb: 'foo,0,0' })).toThrow('must be an integer 0-255');
	});

	it('rejects bare boolean --rgb', () => {
		expect(() => parseUserPreferencesOptions({ rgb: true })).toThrow('--rgb requires r,g,b');
	});

	it('rejects bare boolean --light', () => {
		expect(() => parseUserPreferencesOptions({ light: true })).toThrow('--light requires a mode name');
	});

	it('parses only --light correctly', () => {
		const result = parseUserPreferencesOptions({ light: 'static' });
		expect(result).toEqual({ lightMode: LightMode.Static });
	});

	it('returns empty object when no flags given', () => {
		expect(parseUserPreferencesOptions({})).toEqual({});
	});
});
