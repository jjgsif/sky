enum FrameType {
    Invoke = 0x01,
    ResponseHead = 0x02,
    ResponseChunk = 0x03,
    ResponseEnd = 0x04,
    Error = 0x05,
    Ping = 0x06,
    Pong = 0x07,
    Drain = 0x08,
    InvokeBodyChunk = 0x09,
    InvokeEnd = 0x0A,
    InvokeCredit = 0x0B,
    InvokeCancel = 0x0C,
    ResponseCredit = 0x0D,
    AuthChallenge = 0x0E,
    AuthResponse  = 0x0F,
}

interface FrameHeader {
    version: number;
    frameType: number;
    requestId: number;
    payloadLen: number;
}

interface SkyInvocation {
    requestId: number;
    handlerId: string;
    method: string;
    path: string;
    params: Record<string, string>;
    query: Record<string, string>;
    headers: Record<string, string>;
    body: Uint8Array;
    streamedBody: AsyncIterable<Uint8Array> | null
}


const PROTOCOL_VERSION = 0x01;
const HEADER_SIZE = 10;

type InvocationResult =
    | { ok: true; inv: SkyInvocation }
    | { ok: false; reason: "drain" | "socket_closed" };

type InvocationWaiter = (result: InvocationResult) => void;


export {
    FrameType,
    PROTOCOL_VERSION,
    HEADER_SIZE
};
export type {
    InvocationResult,
    InvocationWaiter,
    FrameHeader,
    SkyInvocation
};
