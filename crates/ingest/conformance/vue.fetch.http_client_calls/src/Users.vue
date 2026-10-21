<template><ul></ul></template>
<script setup lang="ts">
export function load() { return fetch("/api/users"); }
</script>
