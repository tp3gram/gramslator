#!/usr/bin/env python3
"""
WebSocket server that accepts audio uploads from the ESP32 gramslator client
and writes the raw PCM data to a WAV file.

The client connects to WS_PATH with query params including encoding and
sample_rate, then streams PCM audio in binary WebSocket frames. Audio params
are parsed from the query string so the output WAV header is always correct.

Usage:
    python scripts/websocket_audio_server.py [--host 0.0.0.0] [--port 8080] [--output audio_out.wav]

Build the client with:
    DEEPGRAM_HOST=<this-machine-ip> DEEPGRAM_USE_TLS=false DEEPGRAM_PORT=8080 cargo build --release
"""

import argparse
import asyncio
import io
import wave
from typing import Tuple
from urllib.parse import urlparse, parse_qs

import websockets

WS_PATH = "/v2/listen"

ENCODING_SAMPLE_WIDTHS = {
    "linear16": 2,
    "linear32": 4,
}
CHANNELS = 1


def write_wav(path: str, pcm_data: bytes, sample_rate: int, sample_width: int) -> None:
    with wave.open(path, "wb") as wf:
        wf.setnchannels(CHANNELS)
        wf.setsampwidth(sample_width)
        wf.setframerate(sample_rate)
        wf.writeframes(pcm_data)


def parse_audio_params(path: str) -> Tuple[int, int]:
    """Extract sample_rate and sample_width from the WebSocket upgrade URL."""
    qs = parse_qs(urlparse(path).query)
    sample_rate = int(qs.get("sample_rate", [8000])[0])
    encoding = qs.get("encoding", ["linear16"])[0]
    sample_width = ENCODING_SAMPLE_WIDTHS.get(encoding, 2)
    return sample_rate, sample_width


async def serve(host: str, port: int, output: str) -> None:
    done = asyncio.Event()

    async def handler(websocket):
        path = websocket.request.path
        if not path.startswith(WS_PATH):
            print(f"Rejected connection to {path} (only {WS_PATH} is supported)")
            await websocket.close(1008, f"Only {WS_PATH} is supported")
            return

        print(f"Client connected: {websocket.remote_address}  path={path}")

        sample_rate, sample_width = parse_audio_params(path)
        print(f"  Audio params: sample_rate={sample_rate}, sample_width={sample_width}")

        pcm_buf = io.BytesIO()
        frame_count = 0

        try:
            while True:
                try:
                    message = await asyncio.wait_for(websocket.recv(), timeout=5.0)
                except asyncio.TimeoutError:
                    print("  No data for 5 seconds, saving and closing.")
                    break

                if isinstance(message, bytes):
                    pcm_buf.write(message)
                    frame_count += 1

                    print(f"  Received {frame_count} frames ({pcm_buf.tell()} bytes)")
                elif isinstance(message, str):
                    print(f"  Text frame: {message}")
                    if "CloseStream" in message:
                        print("  CloseStream received, closing.")
                        break
        except websockets.exceptions.ConnectionClosed as e:
            print(f"  Connection closed: {e}")

        total = pcm_buf.tell()
        print(f"Done — {frame_count} binary frames, {total} bytes of PCM data.")

        pcm_data = pcm_buf.getvalue()
        if pcm_data:
            write_wav(output, pcm_data, sample_rate, sample_width)
            duration = len(pcm_data) / (sample_rate * sample_width * CHANNELS)
            print(f"Wrote {output} ({len(pcm_data)} bytes, {duration:.1f}s)")
        else:
            print("No audio data received.")

        done.set()

    server = await websockets.serve(handler, host, port)
    print(f"Listening on ws://{host}:{port}{WS_PATH}")
    print("Waiting for a client connection... (Ctrl+C to quit)\n")

    await done.wait()
    server.close()
    await server.wait_closed()


def main():
    parser = argparse.ArgumentParser(description="WebSocket audio receiver for gramslator")
    parser.add_argument("--host", default="0.0.0.0", help="Bind address (default: 0.0.0.0)")
    parser.add_argument("--port", type=int, default=8080, help="Bind port (default: 8080)")
    parser.add_argument("--output", default="audio_out.wav", help="Output WAV file (default: audio_out.wav)")
    args = parser.parse_args()

    asyncio.run(serve(args.host, args.port, args.output))


if __name__ == "__main__":
    main()
