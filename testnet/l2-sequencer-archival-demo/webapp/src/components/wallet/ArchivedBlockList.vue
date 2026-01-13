<template>
  <div class="block-list space-y-12">
    <div 
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
            <span class="text-[10px] text-gray-400 uppercase font-bold tracking-widest">L1 block ID</span>
            <span 
              class="text-xs font-mono text-blue-500 hover:text-blue-700 dark:text-blue-400 dark:hover:text-blue-300 truncate max-w-[150px] sm:max-w-xs cursor-pointer underline decoration-dotted"
              @click="emit('openBlock', block.l1_block_id)"
              :title="`View block ${block.l1_block_id} in explorer`"
            >
              {{ block.l1_block_id }}
            </span>
          </div>
          <div class="flex flex-col pl-5">
            <span class="text-[10px] text-gray-400 uppercase font-bold tracking-widest">L1 tx ID</span>
            <span 
              class="text-xs font-mono text-blue-500 hover:text-blue-700 dark:text-blue-400 dark:hover:text-blue-300 truncate max-w-[150px] sm:max-w-xs cursor-pointer underline decoration-dotted"
              @click="emit('openTransaction', block.l1_transaction_id)"
              :title="`View transaction ${block.l1_transaction_id} in explorer`"
            >
              {{ block.l1_transaction_id }}
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
        @selectTransaction="(tx) => emit('openTransaction', tx.l1_transaction_id)"
        class="mt-0" 
      />
    </div>

    <!-- Empty state when no blocks -->
    <div 
      v-if="!blocks || blocks.length === 0" 
      class="w-full p-1 border border-gray-100 dark:border-gray-400 rounded-2xl"
    >
      <div class="px-6 py-4 text-center text-gray-500 dark:text-gray-400">
        No recent transactions found.
      </div>
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

const emit = defineEmits(['openBlock', 'openTransaction']);
</script>