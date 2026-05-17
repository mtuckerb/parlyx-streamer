export interface Settings {
  parlyx_server_base_url: string;
  api_key: string;
  webhook_url: string | null;
}

export interface AudioDevice {
  name: string;
  channels: number;
  default_sample_rate: number;
  is_default: boolean;
}

export type StreamEvent =
  | { type: "transcript"; text: string; timestamp: string; speaker: string | null; segment_id: string }
  | { type: "partial"; text: string }
  | { type: "diarization"; segments: Array<{ start: number; end: number; speaker: string }> }
  | { type: "speakerrename"; from: string; to: string }
  | { type: "error"; message: string }
  | { type: "complete" };

export type SessionStatus =
  | { status: "Idle" }
  | { status: "Starting" }
  | { status: "Recording"; stream_id: string; task_id: string }
  | { status: "Paused"; stream_id: string; task_id: string }
  | { status: "Stopping" }
  | { status: "Stopped"; task_id: string | null }
  | { status: "Error"; message: string };

export interface Segment {
  id: string;
  speaker: string;
  text: string;
  timestamp: string;
}
