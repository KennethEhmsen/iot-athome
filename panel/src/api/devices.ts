import { request } from "./client";

export interface Device {
  id: string;
  integration: string;
  manufacturer: string;
  model: string;
  label: string;
  rooms: string[];
  capabilities: string[];
  trust_level: "discovered" | "user_added" | "verified";
  last_seen: string;
}

interface ListDevicesResponse {
  devices: Device[];
}

export async function listDevices(): Promise<Device[]> {
  const r = await request<ListDevicesResponse>("/devices");
  return r.devices;
}

export async function getDevice(id: string): Promise<Device> {
  return request<Device>(`/devices/${encodeURIComponent(id)}`);
}
