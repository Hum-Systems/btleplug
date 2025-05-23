// btleplug Source Code File
//
// Copyright 2020 Nonpolynomial Labs LLC. All rights reserved.
//
// Licensed under the BSD 3-Clause license. See LICENSE file in the project root
// for full license information.
//
// Some portions of this file are taken and/or modified from Rumble
// (https://github.com/mwylde/rumble), using a dual MIT/Apache License under the
// following copyright:
//
// Copyright (c) 2014 The Rust Project Developers

use crate::{api::BDAddr, winrtble::utils, Error, Result};
use log::{debug, trace};
use std::future::IntoFuture;
use windows::{
    core::Ref,
    Devices::Bluetooth::{
        BluetoothCacheMode, BluetoothConnectionStatus, BluetoothLEDevice,
        GenericAttributeProfile::{
            GattCharacteristic, GattCommunicationStatus, GattDescriptor, GattDeviceService,
            GattDeviceServicesResult,
        },
    },
    Foundation::TypedEventHandler,
};

pub type ConnectedEventHandler = Box<dyn Fn(bool) + Send>;

pub struct BLEDevice {
    device: BluetoothLEDevice,
    connection_token: i64,
    services: Vec<GattDeviceService>,
}

impl BLEDevice {
    pub async fn new(
        address: BDAddr,
        connection_status_changed: ConnectedEventHandler,
    ) -> Result<Self> {
        let async_op = BluetoothLEDevice::FromBluetoothAddressAsync(address.into())
            .map_err(|_| Error::DeviceNotFound)?;
        let device = async_op
            .into_future()
            .await
            .map_err(|_| Error::DeviceNotFound)?;
        let connection_status_handler =
            TypedEventHandler::new(move |sender: Ref<BluetoothLEDevice>, _| {
                if let Ok(sender) = sender.ok() {
                    let is_connected = sender
                        .ConnectionStatus()
                        .ok()
                        .map_or(false, |v| v == BluetoothConnectionStatus::Connected);
                    connection_status_changed(is_connected);
                    trace!("state {:?}", sender.ConnectionStatus());
                }

                Ok(())
            });
        let connection_token = device
            .ConnectionStatusChanged(&connection_status_handler)
            .map_err(|_| Error::Other("Could not add connection status handler".into()))?;

        Ok(BLEDevice {
            device,
            connection_token,
            services: vec![],
        })
    }

    async fn get_gatt_services(
        &self,
        cache_mode: BluetoothCacheMode,
    ) -> Result<GattDeviceServicesResult> {
        let winrt_error = |e| Error::Other(format!("{:?}", e).into());
        let async_op = self
            .device
            .GetGattServicesWithCacheModeAsync(cache_mode)
            .map_err(winrt_error)?;
        let service_result = async_op.into_future().await.map_err(winrt_error)?;
        Ok(service_result)
    }

    pub async fn connect(&self) -> Result<()> {
        if self.is_connected().await? {
            return Ok(());
        }

        let service_result = self.get_gatt_services(BluetoothCacheMode::Uncached).await?;
        let status = service_result.Status().map_err(|_| Error::DeviceNotFound)?;
        utils::to_error(status)
    }

    async fn is_connected(&self) -> Result<bool> {
        let winrt_error = |e| Error::Other(format!("{:?}", e).into());
        let status = self.device.ConnectionStatus().map_err(winrt_error)?;

        Ok(status == BluetoothConnectionStatus::Connected)
    }

    pub async fn get_characteristics(
        service: &GattDeviceService,
    ) -> Result<Vec<GattCharacteristic>> {
        let async_result = service
            .GetCharacteristicsWithCacheModeAsync(BluetoothCacheMode::Uncached)?
            .into_future()
            .await?;

        match async_result.Status() {
            Ok(GattCommunicationStatus::Success) => {
                let results = async_result.Characteristics()?;
                debug!("characteristics {:?}", results.Size());
                Ok(results.into_iter().collect())
            }
            Ok(GattCommunicationStatus::ProtocolError) => Err(Error::Other(
                format!(
                    "get_characteristics for {:?} encountered a protocol error",
                    service
                )
                .into(),
            )),
            Ok(status) => {
                debug!("characteristic read failed due to {:?}", status);
                Ok(vec![])
            }
            Err(e) => Err(Error::Other(
                format!("get_characteristics for {:?} failed: {:?}", service, e).into(),
            )),
        }
    }

    pub async fn get_characteristic_descriptors(
        characteristic: &GattCharacteristic,
    ) -> Result<Vec<GattDescriptor>> {
        let async_result = characteristic
            .GetDescriptorsWithCacheModeAsync(BluetoothCacheMode::Uncached)?
            .into_future()
            .await?;
        let status = async_result.Status();
        if status == Ok(GattCommunicationStatus::Success) {
            let results = async_result.Descriptors()?;
            debug!("descriptors {:?}", results.Size());
            Ok(results.into_iter().collect())
        } else {
            Err(Error::Other(
                format!(
                    "get_characteristic_descriptors for {:?} failed: {:?}",
                    characteristic, status
                )
                .into(),
            ))
        }
    }

    pub async fn discover_services(&mut self) -> Result<&[GattDeviceService]> {
        let winrt_error = |e| Error::Other(format!("{:?}", e).into());
        let service_result = self.get_gatt_services(BluetoothCacheMode::Uncached).await?;
        let status = service_result.Status().map_err(winrt_error)?;
        if status == GattCommunicationStatus::Success {
            // We need to convert the IVectorView to a Vec, because IVectorView is not Send and so
            // can't be help past the await point below.
            let services: Vec<_> = service_result
                .Services()
                .map_err(winrt_error)?
                .into_iter()
                .collect();
            self.services = services;
            debug!("services {:?}", self.services.len());
        }
        Ok(self.services.as_slice())
    }
}

impl Drop for BLEDevice {
    fn drop(&mut self) {
        let result = self
            .device
            .RemoveConnectionStatusChanged(self.connection_token);
        if let Err(err) = result {
            debug!("Drop:remove_connection_status_changed {:?}", err);
        }

        self.services.iter().for_each(|service| {
            if let Err(err) = service.Close() {
                debug!("Drop:remove_gatt_Service {:?}", err);
            }
        });

        let result = self.device.Close();
        if let Err(err) = result {
            debug!("Drop:close {:?}", err);
        }
    }
}
