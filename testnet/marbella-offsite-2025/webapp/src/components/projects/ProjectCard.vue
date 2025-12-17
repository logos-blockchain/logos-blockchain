<template>
  <div class="project-card" :class="{ featured: project.featured }">
    <h3>{{ project.title }}</h3>
    <p>{{ project.brief }}</p>
  </div>
</template>

<script setup>
  defineProps({
    project: {
      type: Object,
      required: true,
    },
  });
</script>

<style scoped>
  .project-card {
    border: 1px solid #ddd;
    padding: 20px;
    margin-bottom: 20px;
  }

  .featured {
    background: red;
  }
</style>

