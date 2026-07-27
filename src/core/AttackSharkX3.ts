// noinspection JSUnusedGlobalSymbols

import * as HID from 'node-hid';
import { HIDAsync } from 'node-hid';
import { EventEmitter } from 'node:events';
import { ControlTransferError, DeviceError, DriverError, TimeoutError } from '../errors.js';
import { CustomMacroBuilder, type CustomMacroBuilderOptions, MacroMode } from '../protocols/CustomMacroBuilder.js';
import { DpiBuilder, type DpiBuilderOptions } from '../protocols/DpiBuilder.js';
import { InternalStateResetReportBuilder } from '../protocols/InternalStateResetReportBuilder.js';
import { type MacroBuilderOptions, MacrosBuilder } from '../protocols/MacrosBuilder.js';
import { PollingRateBuilder, type Rate } from '../protocols/PollingRateBuilder.js';
import { UserPreferencesBuilder, type UserPreferencesBuilderOptions } from '../protocols/UserPreferencesBuilder.js';
import { Button, TransportKind, type Logger, type TransportOptions } from '../types.js';
import { bufferStartsWith } from '../utils/bufferUtils.js';
import { ConsoleLogger } from '../logger/index.js';
import { delay } from '../utils/delay.js';

const VID = 0x1d57;
const DEVICE_INTERFACE = 2;

/**
 * Events emitted by the AttackSharkX3 class.
 */
export interface AttackSharkX3Events {
	/** Emitted when the battery level changes */
	batteryChange: [battery: number];
	/** Emitted when a data monitoring error occurs */
	error: [error: Error];
}

/**
 * Main driver for the Attack Shark X3/M600 mouse.
 * This class manages the USB connection, DPI settings, polling rate, macros, and user preferences.
 *
 * @example
 * ```TypeScript
 * const driver = new AttackSharkX3({ transport: { kind: TransportKind.Receiver } });
 * await driver.open();
 * const battery = await driver.getBatteryLevel();
 * console.log(`Battery: ${battery}%`);
 * await driver.close();
 * ```
 */
export class AttackSharkX3 extends EventEmitter<AttackSharkX3Events> {
	private readonly productId: number;
	public readonly transport: TransportKind;
	private devicePath: string | undefined;
	private hidDevice?: HIDAsync;
	/**
	 * Delay in milliseconds between packets to prevent the device from locking up.
	 */
	public readonly delayMs: number;
	private isOpen: boolean = false;
	private lastBattery: number = -1;
	private logger: Logger;

	/**
	 * @param options Configuration options for the driver
	 * @param options.transport Transport kind and optional HID path
	 * @param options.logger Optional custom logger
	 * @param options.delayMs Optional delay in milliseconds between packets to prevent lock-up (default: 250)
	 */
	constructor(options: { transport: TransportOptions; logger?: Logger; delayMs?: number }) {
		super();
		if (!options.transport?.kind) {
			throw new DriverError('The transport was not specified');
		}

		this.transport = options.transport.kind;
		this.productId = this.transport === TransportKind.Wired ? 0xfa61 : 0xfa60;
		this.devicePath = options.transport.path;
		this.logger = options.logger ?? new ConsoleLogger();
		this.delayMs = options.delayMs ?? 250;
	}

	async open(): Promise<void> {
		try {
			if (!this.devicePath) {
				const devices = await HID.devicesAsync();
				const candidates = devices.filter((d) => d.vendorId === VID && d.productId === this.productId);
				let deviceInfo: HID.Device | undefined;

				if (this.transport === TransportKind.Wired) {
					// FA61 exposes multiple interface 2 collections (Col01-Col04).
					// Feature reports work on Col04 — prefer that path first.
					deviceInfo = candidates.find(
						(d) => d.interface === DEVICE_INTERFACE && /col04/i.test(d.path ?? ''),
					);
					if (!deviceInfo) {
						deviceInfo = candidates.find((d) => d.interface === DEVICE_INTERFACE);
					}
					if (!deviceInfo) {
						deviceInfo = candidates.find((d) => /col04/i.test(d.path ?? ''));
					}
					if (!deviceInfo) {
						deviceInfo = candidates.find((d) => d.path !== null && d.path !== undefined && d.path !== '');
					}
				} else {
					// FA60 receiver feature reports use interface 2.
					deviceInfo = candidates.find((d) => d.interface === DEVICE_INTERFACE);
				}

				if (!deviceInfo?.path) {
					throw new DriverError(`Device for ${this.transport} transport not found`);
				}
				this.devicePath = deviceInfo.path;
			}

			const devicePath = this.devicePath;
			if (!devicePath) throw new DriverError(`Device for ${this.transport} transport not found`);
			this.hidDevice = await HIDAsync.open(devicePath);
		} catch (e: unknown) {
			if (e instanceof DriverError) throw e;
			throw new DeviceError(`An unexpected error occurred while trying to open ${this.transport} device`, {
				cause: e,
			});
		}

		this.setupListeners();
		this.isOpen = true;
	}

	private setupListeners(): void {
		if (!this.hidDevice) return;

		this.hidDevice.on('error', (err: Error) => {
			// Suppress "could not read" errors if they are expected on some Windows HID collections
			if (err.message.includes('could not read')) {
				this.logger.debug('Suppressed HID read error:', err.message);
				return;
			}
			this.emit('error', err);
		});

		this.on('newListener', (event) => {
			if (
				this.transport === TransportKind.Receiver &&
				event === 'batteryChange' &&
				this.listenerCount('batteryChange') === 0
			) {
				this.hidDevice?.on('data', this.handleData);
				this.startPolling();
			}
		});

		this.on('removeListener', (event) => {
			if (event === 'batteryChange' && this.listenerCount('batteryChange') === 0) {
				this.stopPolling();
				this.hidDevice?.removeListener('data', this.handleData);
			}
		});
	}

	private handleData = (data: Buffer): void => {
		if (data.length < 5) return;
		let battery: number | undefined;
		if (bufferStartsWith(data, Buffer.from([0x03, 0x10, 0x40, 0x01]))) {
			const raw = data[4];
			if (raw !== undefined && raw >= 1 && raw <= 10) {
				battery = raw * 10;
			}
		} else if (bufferStartsWith(data, Buffer.from([0x03, 0x55, 0x40, 0x01]))) {
			battery = data[4];
		}
		if (battery !== undefined && battery !== this.lastBattery) {
			this.lastBattery = battery;
			this.emit('batteryChange', battery);
		}
	};

	private startPolling(): void {
		if (!this.isOpen || !this.hidDevice) return;
		try {
			this.hidDevice.resume();
		} catch (e) {
			this.logger.error('Failed to start polling', e);
		}
	}

	private stopPolling(): void {
		if (!this.hidDevice) return;
		try {
			this.hidDevice.pause();
		} catch {
			/* empty */
		}
	}

	/**
	 * Closes the connection to the device, stops polling, and releases the interfaces.
	 * It is important to call this method when finishing use to avoid resource leaks.
	 */
	async close(): Promise<void> {
		if (!this.isOpen) return;

		this.removeAllListeners();

		try {
			await this.hidDevice?.close();
		} catch {
			/* empty */
		}

		this.isOpen = false;
	}

	private checkIsOpen(): void {
		if (!this.isOpen || !this.hidDevice) throw new DriverError('You have to open the device first');
	}

	private async sendFeatureReport(buffer: Buffer): Promise<number | undefined> {
		this.checkIsOpen();

		try {
			return await this.hidDevice?.sendFeatureReport(buffer);
		} catch (err) {
			throw new ControlTransferError('Control transfer (sendFeatureReport) failed', { cause: err });
		}
	}

	/**
	 * Reads battery telemetry from the FA60 receiver transport.
	 * FA61 wired mode does not expose a battery level.
	 */
	getBatteryLevel(timeoutMs = 1000): Promise<number> {
		this.checkIsOpen();
		if (this.transport === TransportKind.Wired) return Promise.resolve(-1);

		const { promise, resolve, reject } = Promise.withResolvers<number>();
		let finished = false;
		const cleanup = (): void => {
			if (finished) return;
			finished = true;
			clearTimeout(timeout);
			this.removeListener('batteryChange', handleBattery);
		};

		const handleBattery = (battery: number): void => {
			if (finished || battery > 100) return;
			cleanup();
			resolve(battery);
		};

		const timeout = setTimeout(() => {
			cleanup();
			reject(new TimeoutError('Timeout waiting for battery report'));
		}, timeoutMs);
		this.on('batteryChange', handleBattery);

		if (this.lastBattery !== -1 && this.lastBattery <= 100) {
			cleanup();
			resolve(this.lastBattery);
		}

		return promise;
	}

	onBatteryChange(listener: (battery: number) => void): () => void {
		this.checkIsOpen();
		if (this.transport !== TransportKind.Receiver) return () => undefined;

		this.on('batteryChange', listener);

		return () => {
			this.removeListener('batteryChange', listener);
		};
	}

	/**
	 * Sets the polling rate of the mouse.
	 *
	 * @param rate A value from the Rate enum or a PollingRateBuilder instance.
	 * @returns The result of the USB control transfer.
	 *
	 * @example
	 * ```TypeScript
	 * await driver.setPollingRate(Rate.eSports); // 1000Hz
	 * ```
	 */
	setPollingRate(rate: Rate | PollingRateBuilder): Promise<number | undefined> {
		this.checkIsOpen();
		const builder = rate instanceof PollingRateBuilder ? rate : new PollingRateBuilder().setRate(rate);

		return this.sendFeatureReport(builder.build(this.transport));
	}

	/**
	 * Configures an advanced custom macro with multiple events and repetitions.
	 *
	 * @param options CustomMacroBuilder instance or configuration options.
	 *
	 * @example
	 * ```TypeScript
	 * const builder = new CustomMacroBuilder()
	 *   .setPlayOptions(MacroMode.THE_NUMBER_OF_TIME_TO_PLAY, 5)
	 *   .setTargetButton(Button.BACKWARD, macroBuilder)
	 *   .addEvent(KeyCode.A)
	 *   .addEvent(KeyCode.A, 10, true); // Release key A after 10ms
	 * await driver.setCustomMacro(builder);
	 * ```
	 */
	async setCustomMacro(
		options: CustomMacroBuilder | CustomMacroBuilderOptions,
	): Promise<[number | undefined, number | undefined, number | undefined, number | undefined]> {
		this.checkIsOpen();
		const builder = options instanceof CustomMacroBuilder ? options : new CustomMacroBuilder(options);
		const [setMacroBuffer, secondPacket, thirdPacket, fourthPacket] = builder.build(this.transport);

		const responseMacros = await this.sendFeatureReport(setMacroBuffer);
		await delay(this.delayMs);

		const responseSecondPacket = await this.sendFeatureReport(secondPacket);
		await delay(this.delayMs);

		const responseThirdPacket = await this.sendFeatureReport(thirdPacket);
		await delay(this.delayMs);

		const responseFourthPacket = await this.sendFeatureReport(fourthPacket);

		return [responseMacros, responseSecondPacket, responseThirdPacket, responseFourthPacket];
	}

	/**
	 * Maps mouse buttons to simple macros or keyboard functions.
	 *
	 * @param config MacrosBuilder instance or mapping options.
	 *
	 * @example
	 * ```TypeScript
	 * const macroBuilder = new MacrosBuilder().setMacro(Button.DPI, macroTemplates[MacroName.SHORTCUT_SWAP_WINDOW]);
	 * await driver.setMacro(macroBuilder);
	 * ```
	 */
	setMacro(config: MacroBuilderOptions | MacrosBuilder): Promise<number | undefined> {
		this.checkIsOpen();
		const builder = config instanceof MacrosBuilder ? config : new MacrosBuilder(config);

		return this.sendFeatureReport(builder.build(this.transport));
	}

	/**
	 * Sets user preferences, such as lighting, key response time, and sleep timers.
	 *
	 * @param options UserPreferencesBuilder instance or configuration options.
	 *
	 * @example
	 * ```TypeScript
	 * await driver.setUserPreferences({
	 *   lightMode: LightMode.Neon,
	 *   ledSpeed: 5,
	 *   keyResponse: 4
	 * });
	 * ```
	 */
	setUserPreferences(options: UserPreferencesBuilder | UserPreferencesBuilderOptions): Promise<number | undefined> {
		this.checkIsOpen();
		const builder = options instanceof UserPreferencesBuilder ? options : new UserPreferencesBuilder(options);

		return this.sendFeatureReport(builder.build(this.transport));
	}

	sendInternalStateResetReportBuilder(): Promise<number | undefined> {
		this.checkIsOpen();
		const builder = new InternalStateResetReportBuilder();

		return this.sendFeatureReport(builder.build(this.transport));
	}

	resetPollingRate(): Promise<number | undefined> {
		this.checkIsOpen();
		const builder = new PollingRateBuilder();

		return this.sendFeatureReport(builder.build(this.transport));
	}

	/**
	 * Configures the DPI stages and values for the mouse.
	 *
	 * @param options DpiBuilder instance or configuration options.
	 * @returns The result of the USB control transfer.
	 *
	 * @example
	 * ```TypeScript
	 * const dpiBuilder = new DpiBuilder({
	 *   dpiValues: [800, 1600, 2400, 3200, 5000, 22000],
	 *   activeStage: 2
	 * });
	 * await driver.setDpi(dpiBuilder);
	 * ```
	 */
	setDpi(options: DpiBuilder | DpiBuilderOptions): Promise<number | undefined> {
		this.checkIsOpen();
		let builder: DpiBuilder;
		if (options instanceof DpiBuilder) {
			builder = options;
		} else {
			const merged = { ...DpiBuilder.X3_DEFAULT_OPTIONS, ...options };
			builder = new DpiBuilder(merged);
		}

		return this.sendFeatureReport(builder.build(this.transport));
	}

	resetDpi(): Promise<number | undefined> {
		this.checkIsOpen();
		const builder = new DpiBuilder(DpiBuilder.X3_DEFAULT_OPTIONS);

		return this.sendFeatureReport(builder.build(this.transport));
	}

	resetMacro(): Promise<number | undefined> {
		this.checkIsOpen();
		const builder = new MacrosBuilder();

		return this.sendFeatureReport(builder.build(this.transport));
	}

	resetCustomMacro(): Promise<[number | undefined, number | undefined, number | undefined, number | undefined]> {
		this.checkIsOpen();
		const builder = new CustomMacroBuilder({
			playOptions: {
				mode: MacroMode.THE_NUMBER_OF_TIME_TO_PLAY,
				times: 1,
			},
			targetButton: Button.BACKWARD,
			macroEvents: [],
		});

		return this.setCustomMacro(builder);
	}

	resetUserPreferences(): Promise<number | undefined> {
		this.checkIsOpen();
		const builder = new UserPreferencesBuilder({ rgb: { r: 0, g: 0, b: 255 } }).setKeyResponse(8);

		return this.sendFeatureReport(builder.build(this.transport));
	}

	/**
	 * Resets the mouse to factory settings (all profiles and definitions).
	 *
	 * Packets are sent with `delayMs` spacing so the device has time to process each
	 * report before the next one arrives. Stock FA61/X3 captures show ~500 ms between
	 * reset packets; sending them back-to-back can cause incomplete resets that require
	 * running the sequence twice. The public `delayMs` option is now meaningful for
	 * reset timing as well.
	 *
	 * @returns A promise that resolves when the reset is complete.
	 */
	async reset(): Promise<void> {
		this.checkIsOpen();
		await this.sendInternalStateResetReportBuilder();
		await delay(this.delayMs);
		await this.resetDpi();
		await delay(this.delayMs);
		await this.resetUserPreferences();
		await delay(this.delayMs);
		await this.resetPollingRate();
		await delay(this.delayMs);
		await this.resetMacro();

		// FA61/X3 stock reset and new-profile captures do not send custom-macro definition pages (09).
		// Sending the legacy empty custom macro breaks the back button on X3 hardware, so reset
		// intentionally stops after the standard macro report.
	}
}

export default AttackSharkX3;
