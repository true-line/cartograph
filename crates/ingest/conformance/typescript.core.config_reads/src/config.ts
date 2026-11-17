export function load(name: string) {
  return [process.env.DATABASE_URL, process.env[name]];
}

export function lookalike(process: { env: Record<string, string> }) {
  return process.env.SHADOWED;
}
