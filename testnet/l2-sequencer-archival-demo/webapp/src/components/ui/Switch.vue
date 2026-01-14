<template>
  <Switch
    :modelValue="modelValue"
    @update:modelValue="$emit('update:modelValue', $event)"
    
    :class="{ 
      'bg-transparent': true, // Always transparent background
      // Optional: Add a subtle background color if the switch is ON/Enabled
      // 'bg-gray-200': modelValue, 
    }"
    
    class="relative inline-flex h-6 w-11 shrink-0 cursor-pointer rounded-full border-2 
    
    border-gray-500 transition-colors duration-200 ease-in-out focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-700/75"
  >
    <span class="sr-only">Toggle Theme</span>
    
    <span
      aria-hidden="true"
      :class="modelValue ? 'translate-x-5' : 'translate-x-0'"
      
      class="pointer-events-none inline-block h-5 w-5 transform rounded-full shadow-lg ring-0 transition duration-200 ease-in-out"
    >
      <div class="h-full w-full flex items-center justify-center text-gray-800">
        <template v-if="modelValue">
          <slot name="icon-enabled"></slot>
        </template>
        <template v-else>
          <slot name="icon-disabled"></slot>
        </template>
      </div>
    </span>
  </Switch>
</template>

<script setup>
// ... (script remains the same)
import { Switch } from '@headlessui/vue';

defineProps({
  modelValue: {
    type: Boolean,
    required: true,
  },
});

defineEmits(['update:modelValue']);
</script>
