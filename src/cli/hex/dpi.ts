import { DpiBuilder } from '../../protocols/DpiBuilder.js';
import type { TransportKind } from '../../types.js';
import { HEX_DPI_HELP } from '../help.js';
import { parseActiveStage, parseDpiSensorOptions, parseDpiStages } from '../helpers.js';

export const help = HEX_DPI_HELP;

export function run(transport: TransportKind, flags: Record<string, string | boolean>): void {
	const dpiValues = parseDpiStages(flags['stages']);
	const maxActiveStage = dpiValues.length as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8;
	const activeStage = parseActiveStage(flags['active'], maxActiveStage);
	const sensorOptions = parseDpiSensorOptions(flags);

	const options =
		activeStage !== undefined ? { dpiValues, activeStage, ...sensorOptions } : { dpiValues, ...sensorOptions };
	const builder = new DpiBuilder({ ...DpiBuilder.X3_DEFAULT_OPTIONS, ...options });
	const buffer = builder.build(transport);
	console.log(buffer.toString('hex'));
}
