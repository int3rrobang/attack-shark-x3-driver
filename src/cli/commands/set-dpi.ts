import { ConnectionMode } from '../../types.js';
import { SET_DPI_HELP } from '../help.js';
import { parseActiveStage, parseDpiSensorOptions, parseDpiStages, withDriver } from '../helpers.js';

export const help = SET_DPI_HELP;

export async function run(
	mode: ConnectionMode,
	delayMs: number,
	flags: Record<string, string | boolean>,
): Promise<void> {
	const dpiValues = parseDpiStages(flags['stages'], mode);
	const maxActiveStage = (mode === ConnectionMode.X3Wired ? dpiValues.length : 6) as 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8;
	const activeStage = parseActiveStage(flags['active'], maxActiveStage);
	const sensorOptions = parseDpiSensorOptions(flags, mode);

	await withDriver(mode, delayMs, async (driver) => {
		await driver.setDpi(
			activeStage !== undefined ? { dpiValues, activeStage, ...sensorOptions } : { dpiValues, ...sensorOptions },
		);
	});
	console.log('DPI stages set successfully.');
}
