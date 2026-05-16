import type { ApiResult } from "../types";

interface Props<T> {
  result: ApiResult<T>;
  render: (data: T) => React.ReactNode;
}

export function ResultCard<T>({ result, render }: Props<T>) {
  if (result.state === "idle") return null;
  if (result.state === "loading") return <p className="result loading">Loading…</p>;
  if (result.state === "error") return <p className="result error">Error: {result.message}</p>;
  return <div className="result ok">{render(result.data)}</div>;
}
