<template>
  <Switch :modelValue="enabled" @update:modelValue="toggleTheme">
    <template v-slot:icon-enabled>
      <IconMoon class="h-4 w-4 text-gray-400"/> 
    </template>
    <template v-slot:icon-disabled>
      <IconSun class="h-4 w-4"/>
    </template>
  </Switch>
</template>

<script setup>
  import { ref, watchEffect, onMounted, onUnmounted } from 'vue';
  import { 
    Switch,
    IconMoon,
    IconSun,
  } from '@/components';

  const enabled = ref(false);

  const updateTheme = (isDarkMode) => {
    if (isDarkMode) {
      document.documentElement.classList.add('dark');
    } else {
      document.documentElement.classList.remove('dark');
    }
  };

  const toggleTheme = () => {
    enabled.value = !enabled.value;
    updateTheme(enabled.value);
  };

  onMounted(() => {
    enabled.value = window.matchMedia('(prefers-color-scheme: dark)').matches;
    updateTheme(enabled.value)
  });

  const systemPrefersDark = window.matchMedia('(prefers-color-scheme: dark)');
  systemPrefersDark.addEventListener('change', (e) => {
    enabled.value = e.matches;
    updateTheme(e.matches);
  });

  onUnmounted(() => {
    systemPrefersDark.removeEventListener('change', handleSystemThemeChange);
  });
</script>

