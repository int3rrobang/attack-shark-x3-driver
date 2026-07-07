import { DpiBuilder } from '../../protocols/DpiBuilder.js';
import { ConnectionMode } from '../../types.js';
import { HEX_DPI_HELP } from '../help.js';
import { parseActiveStage, parseDpiSensorOptions, parseDpiStages } from '../helpers.js';

export const help = HEX_DPI_HELP;

export function run(mode: ConnectionMode, flags: Record<string, string | boolean>): void {
	const dpiValues = parseDpiStages(flags['stages'], mode);
	const maxActiveStage = (mode === ConnectionMode.X3Wired ? dpiValues.length : 6) as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8;
	const activeStage = parseActiveStage(flags['active'], maxActiveStage);
	const sensorOptions = parseDpiSensorOptions(flags, mode);

	const options =
		activeStage !== undefined ? { dpiValues, activeStage, ...sensorOptions } : { dpiValues, ...sensorOptions };
	const builder = new DpiBuilder(
		mode === ConnectionMode.X3Wired ? { ...DpiBuilder.X3_DEFAULT_OPTIONS, ...options } : options,
	);
	const buffer = builder.build(mode);
	console.log(buffer.toString('hex'));
}
