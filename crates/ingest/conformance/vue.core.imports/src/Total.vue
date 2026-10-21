<template><p>{{ total }}</p></template>
<script setup>
import { sum } from "./math.js";
const total = sum(1, 2);
</script>
