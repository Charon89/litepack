// Listing batches, v1 (crates/lpk-gui/src/batch.rs): a 28-byte header, 32 bytes per row,
// then the UTF-8 names. Little-endian.

const MAGIC = 0x424b504c; // "LPKB" read as a little-endian u32
export const HEADER_LEN = 28;
export const ROW_LEN = 32;

/**
 * @param {ArrayBuffer} buffer
 * @returns {{generation: number, total: number, first: number, rows: Array<object>}}
 */
export function decodeBatch(buffer) {
  const view = new DataView(buffer);
  if (view.getUint32(0, true) !== MAGIC || view.getUint16(4, true) !== 1) {
    throw new Error("not a v1 batch");
  }
  const generation = view.getUint32(8, true);
  const total = view.getUint32(12, true);
  const first = view.getUint32(16, true);
  const count = view.getUint32(20, true);
  const namesAt = HEADER_LEN + count * ROW_LEN;
  const names = new Uint8Array(buffer, namesAt);
  const utf8 = new TextDecoder("utf-8");
  const rows = [];
  for (let i = 0; i < count; i++) {
    const o = HEADER_LEN + i * ROW_LEN;
    const off = view.getUint32(o + 24, true);
    const len = view.getUint32(o + 28, true);
    rows.push({
      id: view.getUint32(o, true),
      kind: view.getUint8(o + 4),
      size: view.getBigUint64(o + 8, true),
      mtimeNs: view.getBigInt64(o + 16, true),
      name: utf8.decode(names.subarray(off, off + len)),
    });
  }
  return { generation, total, first, rows };
}
