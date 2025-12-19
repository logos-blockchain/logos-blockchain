<template>
  <div class="block-list space-y-12"> <div 
      v-for="block in blocks" 
      :key="block.data.block_id" 
      class="block-container pb-20"
    >
      <div class="flex items-center justify-between mb-4 px-2">
        <div class="flex items-center space-x-6">
          <div class="bg-black dark:bg-white text-white dark:text-black px-4 py-1.5 rounded-xl font-mono font-black text-sm shadow-sm mr-5">
            BLOCK #{{ block.data.block_id }}
          </div>
          <div class="flex flex-col pl-5">
            <span class="text-[10px] text-gray-400 uppercase font-bold tracking-widest">L1 Reference</span>
            <span class="text-xs font-mono text-gray-500 truncate max-w-[150px] sm:max-w-xs">
              {{ block.l1_block_id }}
            </span>
          </div>
        </div>
        
        <div class="hidden sm:block text-right">
          <span class="text-[10px] text-gray-400 uppercase font-bold tracking-widest block">Parent</span>
          <span class="text-xs font-mono text-gray-400">#{{ block.data.parent_block_id }}</span>
        </div>
      </div>

      <TransactionList 
        :transactions="block.data.transactions" 
        :currentAccount="currentAccount"
        class="mt-0" 
      />
    </div>
  </div>
</template>

<script setup>
import { TransactionList } from '@/components';

defineProps({
  blocks: {
    type: Array,
    required: true
  },
  currentAccount: {
    type: String,
    default: 'ARCHIVE_VIEW'
  }
});
</script>
