<template>
  <div class="wallet-card flex flex-col p-6 sm:p-8 lg:p-10 
    rounded-2xl transition-all duration-300 w-full max-w-none border border-gray-100 dark:border-gray-400">
    
    <div class="flex items-center justify-between mb-6 border-b pb-4 border-gray-200 dark:border-gray-700">
      <h2 class="text-2xl sm:text-3xl font-bold text-gray-900 dark:text-white">
        Wallet Overview
      </h2>
      
      <div class="flex items-center space-x-1 bg-gray-50 dark:bg-gray-800/50 p-1 rounded-xl">
        <button 
          @click="emit('prev')"
          class="p-2 rounded-lg hover:bg-white dark:hover:bg-gray-700 hover:shadow-sm transition-all group"
          title="Previous Character"
        >
          <svg xmlns="http://www.w3.org/2000/svg" class="h-5 w-5 text-gray-400 group-hover:text-gray-900 dark:group-hover:text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M15 19l-7-7 7-7" />
          </svg>
        </button>

        <button 
          @click="emit('regenerate')"
          class="p-2 rounded-lg hover:bg-white dark:hover:bg-gray-700 hover:shadow-sm transition-all group"
          title="Random Character"
        >
          <svg xmlns="http://www.w3.org/2000/svg" class="h-5 w-5 text-gray-400 group-hover:text-gray-900 dark:group-hover:text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" />
          </svg>
        </button>

        <button 
          @click="emit('next')"
          class="p-2 rounded-lg hover:bg-white dark:hover:bg-gray-700 hover:shadow-sm transition-all group"
          title="Next Character"
        >
          <svg xmlns="http://www.w3.org/2000/svg" class="h-5 w-5 text-gray-400 group-hover:text-gray-900 dark:group-hover:text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9 5l7 7-7 7" />
          </svg>
        </button>
      </div>
    </div>

    <div class="space-y-6">
      <div class="flex flex-col">
        <span class="text-sm font-semibold text-gray-500 dark:text-gray-400 mb-1">
          Character Name
        </span>
        <p class="text-base sm:text-lg font-mono break-all text-gray-700 dark:text-white">
          {{ walletAddress }}
        </p>
      </div>

      <div class="flex flex-col pt-4">
        <span class="text-sm font-semibold text-gray-500 dark:text-gray-400 mb-2">
          Current Balance
        </span>
        <p class="text-4xl sm:text-5xl font-extrabold text-gray-900 dark:text-white">
          {{ walletAmount }} 
          <span class="text-xl font-medium text-gray-500 dark:text-gray-400 ps-2">
            MEM
          </span>
        </p>
      </div>
    </div>
  </div>
</template>

<script setup>
defineProps({
  walletAddress: {
    type: String,
    required: true,
  },
  walletAmount: {
    type: [String, Number],
    required: true,
  },
});

const emit = defineEmits(['regenerate', 'prev', 'next']);
</script>
