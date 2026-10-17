// Sending a file to the engine host (RFC 0011): `sample.upload` in chunks
// that fit a WebSocket frame, so it works with a remote engine and with
// files of any size the engine takes. Returns the finished upload's id,
// which `sampler.load` takes instead of a path.

import { MAX_UPLOAD_BYTES } from "./generated/limits";
import { act, client } from "./store";

/** Base64 of some bytes. */
function base64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

/** Send `file` to the engine host; its upload id, or null if refused (the
 * error is shown). */
export async function uploadFile(file: File): Promise<string | null> {
  const upload = `ui-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
  const size = file.size;
  for (let offset = 0; offset < size || offset === 0; offset += MAX_UPLOAD_BYTES) {
    const chunk = new Uint8Array(await file.slice(offset, offset + MAX_UPLOAD_BYTES).arrayBuffer());
    const done = offset + MAX_UPLOAD_BYTES >= size;
    const r = await act(client.call("sample.upload", { upload, name: file.name, offset, data: base64(chunk), done }));
    if (!r) return null;
    if (done) break;
  }
  return upload;
}
