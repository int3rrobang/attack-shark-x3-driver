use std::time::Duration;

use windows::{
    Devices::Bluetooth::{
        BluetoothCacheMode, BluetoothLEDevice,
        GenericAttributeProfile::{
            GattCharacteristic, GattCharacteristicProperties,
            GattClientCharacteristicConfigurationDescriptorValue, GattCommunicationStatus,
            GattDeviceService, GattOpenStatus, GattSharingMode, GattValueChangedEventArgs,
            GattWriteOption,
        },
    },
    Foundation::{EventRegistrationToken, TypedEventHandler},
    Storage::Streams::{DataReader, DataWriter},
    core::{GUID, HSTRING},
};

use super::{
    BleError, BleWriteReceipt, NotificationInbox, NotificationSender, complete_ble_write,
    notification_channel,
};

/// A Windows GATT session with an explicit shared read/write claim on FEE0.
pub(super) struct WindowsGattSession {
    _device: BluetoothLEDevice,
    _service: GattDeviceService,
    write_characteristic: GattCharacteristic,
    ack_characteristic: GattCharacteristic,
    notifications: NotificationInbox,
    notification_token: Option<EventRegistrationToken>,
}

impl WindowsGattSession {
    pub(super) async fn open(
        device_id: &str,
        service_uuid: u128,
        write_uuid: u128,
        ack_uuid: u128,
    ) -> Result<Self, BleError> {
        let device = BluetoothLEDevice::FromIdAsync(&HSTRING::from(device_id))
            .map_err(|error| operation("create BluetoothLEDevice request", error))?
            .await
            .map_err(|error| operation("open BluetoothLEDevice", error))?;
        let result = device
            .GetGattServicesForUuidWithCacheModeAsync(
                GUID::from_u128(service_uuid),
                BluetoothCacheMode::Uncached,
            )
            .map_err(|error| operation("create FEE0 discovery request", error))?
            .await
            .map_err(|error| operation("discover FEE0 service", error))?;
        check_status(
            result
                .Status()
                .map_err(|error| operation("read FEE0 discovery status", error))?,
            "discover FEE0 service",
        )?;
        let services = result
            .Services()
            .map_err(|error| operation("read discovered FEE0 services", error))?;
        let mut services = services.into_iter();
        let service = services.next().ok_or(BleError::MissingService {
            uuid: super::fee0_service(),
        })?;
        if services.next().is_some() {
            return Err(BleError::AmbiguousService {
                uuid: super::fee0_service(),
            });
        }

        let open_status = service
            .OpenAsync(GattSharingMode::SharedReadAndWrite)
            .map_err(|error| operation("create shared FEE0 open request", error))?
            .await
            .map_err(|error| operation("open FEE0 for shared read/write access", error))?;
        if open_status != GattOpenStatus::Success && open_status != GattOpenStatus::AlreadyOpened {
            return Err(BleError::Operation {
                operation: "open FEE0 for shared read/write access",
                details: format!("status {open_status:?}"),
            });
        }

        let result = service
            .GetCharacteristicsWithCacheModeAsync(BluetoothCacheMode::Uncached)
            .map_err(|error| operation("create FEE0 characteristic discovery request", error))?
            .await
            .map_err(|error| operation("discover FEE0 characteristics", error))?;
        check_status(
            result
                .Status()
                .map_err(|error| operation("read characteristic discovery status", error))?,
            "discover FEE0 characteristics",
        )?;
        let characteristics = result
            .Characteristics()
            .map_err(|error| operation("read discovered FEE0 characteristics", error))?;
        let write_characteristic = exactly_one_characteristic(
            &characteristics,
            GUID::from_u128(write_uuid),
            super::fee3_write(),
        )?;
        let ack_characteristic = exactly_one_characteristic(
            &characteristics,
            GUID::from_u128(ack_uuid),
            super::fee4_ack(),
        )?;
        let write_properties = write_characteristic
            .CharacteristicProperties()
            .map_err(|error| operation("read FEE3 properties", error))?;
        if !write_properties.contains(GattCharacteristicProperties::Write) {
            return Err(BleError::InvalidCharacteristicProperties {
                uuid: super::fee3_write(),
            });
        }
        let ack_properties = ack_characteristic
            .CharacteristicProperties()
            .map_err(|error| operation("read FEE4 properties", error))?;
        if !ack_properties.contains(GattCharacteristicProperties::Notify) {
            return Err(BleError::InvalidCharacteristicProperties {
                uuid: super::fee4_ack(),
            });
        }

        let (notifications, notification_token) = Self::subscribe(&ack_characteristic).await?;
        Ok(Self {
            _device: device,
            _service: service,
            write_characteristic,
            ack_characteristic,
            notifications,
            notification_token: Some(notification_token),
        })
    }

    pub(super) async fn write(
        &mut self,
        report_id: u8,
        packet: &[u8],
        ack_timeout: Duration,
    ) -> Result<BleWriteReceipt, BleError> {
        self.notifications.drain_idle()?;
        let write_characteristic = &self.write_characteristic;
        complete_ble_write(
            report_id,
            ack_timeout,
            &mut self.notifications,
            Self::write_packet(write_characteristic, packet),
        )
        .await
    }

    async fn subscribe(
        ack_characteristic: &GattCharacteristic,
    ) -> Result<(NotificationInbox, EventRegistrationToken), BleError> {
        let (sender, receiver) = notification_channel();
        let token = ack_characteristic
            .ValueChanged(&TypedEventHandler::new(
                move |_characteristic, event_args: &Option<GattValueChangedEventArgs>| {
                    send_notification(&sender, event_args.as_ref());
                    Ok(())
                },
            ))
            .map_err(|error| operation("register FEE4 notification handler", error))?;
        let result = ack_characteristic
            .WriteClientCharacteristicConfigurationDescriptorWithResultAsync(
                GattClientCharacteristicConfigurationDescriptorValue::Notify,
            )
            .map_err(|error| operation("create FEE4 notification subscription request", error))?
            .await
            .map_err(|error| operation("enable FEE4 notifications", error));
        let result = match result {
            Ok(result) => check_status(
                result
                    .Status()
                    .map_err(|error| operation("read FEE4 subscription status", error))?,
                "enable FEE4 notifications",
            ),
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            let _ = ack_characteristic.RemoveValueChanged(token);
            return Err(error);
        }
        Ok((receiver, token))
    }

    async fn write_packet(
        write_characteristic: &GattCharacteristic,
        packet: &[u8],
    ) -> Result<(), BleError> {
        let writer =
            DataWriter::new().map_err(|error| operation("create FEE3 packet writer", error))?;
        writer
            .WriteBytes(packet)
            .map_err(|error| operation("buffer FEE3 packet", error))?;
        let buffer = writer
            .DetachBuffer()
            .map_err(|error| operation("finish FEE3 packet buffer", error))?;
        let result = write_characteristic
            .WriteValueWithResultAndOptionAsync(&buffer, GattWriteOption::WriteWithResponse)
            .map_err(|error| operation("create FEE3 write request", error))?
            .await
            .map_err(|error| operation("write packet to FEE3", error))?;
        check_status(
            result
                .Status()
                .map_err(|error| operation("read FEE3 write status", error))?,
            "write packet to FEE3",
        )
    }

    pub(super) async fn close(&mut self) -> Result<(), BleError> {
        let Some(token) = self.notification_token.take() else {
            return Ok(());
        };
        let result = self
            .ack_characteristic
            .WriteClientCharacteristicConfigurationDescriptorWithResultAsync(
                GattClientCharacteristicConfigurationDescriptorValue::None,
            )
            .map_err(|error| operation("create FEE4 notification disable request", error))?
            .await
            .map_err(|error| operation("disable FEE4 notifications", error))
            .and_then(|result| {
                check_status(
                    result.Status().map_err(|error| {
                        operation("read FEE4 notification disable status", error)
                    })?,
                    "disable FEE4 notifications",
                )
            });
        let remove_result = self
            .ack_characteristic
            .RemoveValueChanged(token)
            .map_err(|error| operation("remove FEE4 notification handler", error));
        result?;
        remove_result
    }
}

impl Drop for WindowsGattSession {
    fn drop(&mut self) {
        if let Some(token) = self.notification_token.take() {
            let _ = self.ack_characteristic.RemoveValueChanged(token);
        }
    }
}

fn send_notification(sender: &NotificationSender, event_args: Option<&GattValueChangedEventArgs>) {
    let value = event_args
        .ok_or_else(|| {
            BleError::Notification("FEE4 notification had no event arguments".to_owned())
        })
        .and_then(|event_args| read_notification(event_args).map_err(BleError::Notification));
    sender.send(value);
}

fn exactly_one_characteristic(
    characteristics: &windows::Foundation::Collections::IVectorView<GattCharacteristic>,
    target: GUID,
    uuid: bluest::Uuid,
) -> Result<GattCharacteristic, BleError> {
    let mut matches = characteristics
        .into_iter()
        .filter(|characteristic| characteristic.Uuid().is_ok_and(|value| value == target));
    let characteristic = matches
        .next()
        .ok_or(BleError::MissingCharacteristic { uuid })?;
    if matches.next().is_some() {
        return Err(BleError::AmbiguousCharacteristic { uuid });
    }
    Ok(characteristic)
}

fn read_notification(event_args: &GattValueChangedEventArgs) -> Result<Vec<u8>, String> {
    let buffer = event_args
        .CharacteristicValue()
        .map_err(|error| format!("read FEE4 notification buffer: {error}"))?;
    let mut value =
        vec![
            0;
            buffer
                .Length()
                .map_err(|error| format!("read FEE4 notification length: {error}"))?
                .try_into()
                .map_err(|error| format!("convert FEE4 notification length: {error}"))?
        ];
    let reader = DataReader::FromBuffer(&buffer)
        .map_err(|error| format!("create FEE4 notification reader: {error}"))?;
    reader
        .ReadBytes(&mut value)
        .map_err(|error| format!("read FEE4 notification bytes: {error}"))?;
    Ok(value)
}

fn check_status(
    status: GattCommunicationStatus,
    operation_name: &'static str,
) -> Result<(), BleError> {
    if status == GattCommunicationStatus::Success {
        Ok(())
    } else {
        Err(BleError::Operation {
            operation: operation_name,
            details: format!("GATT status {status:?}"),
        })
    }
}

fn operation(operation_name: &'static str, error: impl std::fmt::Display) -> BleError {
    BleError::Operation {
        operation: operation_name,
        details: error.to_string(),
    }
}
