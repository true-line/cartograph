function load(name) {
  return [process.env.DATABASE_URL, process.env[name]];
}

function lookalike(process) {
  return process.env.SHADOWED;
}

module.exports = { load, lookalike };
