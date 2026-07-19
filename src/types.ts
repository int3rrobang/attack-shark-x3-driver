/**
 * Transport kinds supported by the X3/M600 driver.
 */
export enum TransportKind {
	Wired = 'wired',
	Receiver = 'receiver',
}

/**
 * Configuration for a wired transport.
 */
export interface WiredTransportOptions {
	kind: TransportKind.Wired;
	path?: string;
}

/**
 * Configuration for a 2.4 GHz receiver transport.
 */
export interface ReceiverTransportOptions {
	kind: TransportKind.Receiver;
	path?: string;
}

/**
 * Options for selecting the USB transport and, optionally, a specific HID path.
 */
export type TransportOptions = WiredTransportOptions | ReceiverTransportOptions;

/**
 * Base structure for USB control transfer options.
 */
interface ControlTransferBase {
	/** Request type (bmRequestType) */
	bmRequestType: number;
	/** Specific request (bRequest) */
	bRequest: number;
	/** Request value (wValue) */
	wValue: number;
	/** Request index (wIndex) */
	wIndex: number;
}

/**
 * Options for input control transfer (reading from the device).
 */
export interface ControlTransferIn extends ControlTransferBase {
	/** Size of data to be read */
	data: number;
}

/**
 * Options for output control transfer (writing to the device).
 */
export interface ControlTransferOut extends ControlTransferBase {
	/** Buffer of data to be sent */
	data: Buffer;
}

/**
 * Union of types for control transfer options.
 */
export type ControlTransferOptions = ControlTransferIn | ControlTransferOut;

/**
 * Mapping of physical mouse buttons.
 */
export enum Button {
	/** Main left button */
	LEFT = 0,
	/** Main right button */
	RIGHT = 1,
	/** Middle button (scroll click) */
	MIDDLE = 2,
	/** Forward side button */
	FORWARD = 3,
	/** Backward side button */
	BACKWARD = 4,
	/** DPI adjustment button */
	DPI = 5,
	/** Scroll up */
	SCROLL_UP = 6,
	/** Scroll down */
	SCROLL_DOWN = 7,
}

/**
 * Supported log levels.
 */
export type LogLevel = 'debug' | 'info' | 'warn' | 'error';

/**
 * Interface for the driver's internal logger.
 */
export interface Logger {
	/** Logs a debug message */
	debug(message: string, context?: unknown): void;

	/** Logs an informational message */
	info(message: string, context?: unknown): void;

	/** Logs a warning */
	warn(message: string, context?: unknown): void;

	/** Logs an error */
	error(message: string, context?: unknown): void;
}

export enum ReportId {
	DPI = 0x04,
	POLLING_RATE = 0x06,
	LIGHTING_SETTINGS = 0x05,
	BUTTON_MAPPING = 0x08,
	MACRO = 0x09,
	DEVICE_VERSION = 0x0b,
	READ_REPORT_ID = 0xa0,
	WAKE_UP_MODE = 0x07,
}

export enum PacketLength {
	DPI = 0x38,
	POLLING_RATE = 0x09,
	LIGHTING_SETTINGS = 0x0f,
	BUTTON_MAPPING = 0x3b,
	// eslint-disable-next-line @typescript-eslint/no-duplicate-enum-values
	MACRO = 0x09,
	DEVICE_VERSION = 0x08,
}
