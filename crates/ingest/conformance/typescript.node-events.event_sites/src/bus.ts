import { EventEmitter } from "events";
const bus = new EventEmitter();
export function notify() { bus.emit("order-created", {}); }
