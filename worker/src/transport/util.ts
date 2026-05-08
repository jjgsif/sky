import { HEADER_SIZE, PROTOCOL_VERSION, type FrameHeader } from "./types";

function encodeHeader(h: FrameHeader): Buffer {
  const buf = Buffer.allocUnsafe(HEADER_SIZE);
  buf[0] = h.version;
  buf[1] = h.frameType;
  buf.writeUInt32BE(h.requestId,  2);
  buf.writeUInt32BE(h.payloadLen, 6);
  return buf;
}
 
function decodeHeader(buf: Buffer, offset: number): FrameHeader {
  if (buf[offset] !== PROTOCOL_VERSION) {  // ← was buf[0], should be buf[offset]
    throw new Error(
      `Sky transport: version mismatch — expected ${PROTOCOL_VERSION}, got ${buf[offset]}`
    );
  }
  return {
    version:    buf[offset],
    frameType:  buf[offset + 1],
    requestId:  buf.readUInt32BE(offset + 2),
    payloadLen: buf.readUInt32BE(offset + 6),
  };
}

export { decodeHeader, encodeHeader };
