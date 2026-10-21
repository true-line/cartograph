<template><button @click="run()">Go</button></template>
<script>
function helper() { return 1; }
export function run() { return helper(); }
</script>
