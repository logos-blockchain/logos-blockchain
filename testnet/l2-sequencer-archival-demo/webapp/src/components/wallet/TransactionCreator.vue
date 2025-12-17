<template>
  <div class="send-money-card flex flex-col p-6 sm:p-8 lg:p-10 
    rounded-2xl transition-all duration-300 w-full max-w-none border border-gray-100 dark:border-gray-400">
    
    <div class="flex items-center justify-between mb-6 border-b pb-4 border-gray-200 dark:border-gray-700">
      <h2 class="text-2xl sm:text-3xl font-bold text-gray-900 dark:text-white">
        Send MEM
      </h2>
    </div>

    <form @submit.prevent="handleTransfer" class="space-y-6">
      
      <div class="flex flex-col">
        <label class="text-sm font-semibold text-gray-500 dark:text-gray-400 mb-2">
          To Address
        </label>
        <input 
          v-model="transferData.to"
          type="text" 
          placeholder="Recipient name or 0x..."
          class="w-full p-3 rounded-xl border border-gray-200 dark:border-gray-700 bg-transparent text-gray-700 dark:text-white focus:ring-2 focus:ring-gray-400 outline-none font-mono transition-all"
          required
        />
      </div>

      <div class="flex flex-col">
        <label class="text-sm font-semibold text-gray-500 dark:text-gray-400 mb-2">
          Amount (MEM)
        </label>
        <input 
          v-model.number="transferData.amount"
          type="number" 
          step="0.01"
          placeholder="0.00"
          class="w-full p-3 rounded-xl border border-gray-200 dark:border-gray-700 bg-transparent text-gray-900 dark:text-white focus:ring-2 focus:ring-gray-400 outline-none font-bold text-lg transition-all"
          required
        />
      </div>

      <div class="pt-4">
        <button 
          type="submit"
          class="w-full py-4 rounded-xl bg-gray-900 dark:bg-white text-white dark:text-gray-900 font-bold text-lg hover:opacity-90 active:scale-[0.98] transition-all shadow-lg"
        >
          Confirm Transfer
        </button>
      </div>
      
    </form>
  </div>
</template>

<script setup>
import { reactive, defineEmits } from 'vue';

const props = defineProps({
  fromAddress: {
    type: String,
    default: 'Alisa'
  }
});

const emit = defineEmits(['transfer']);

const transferData = reactive({
  to: '',
  amount: null
});

const handleTransfer = () => {
  if (!transferData.to || !transferData.amount) return;
  
  // Include the 'from' address from props when emitting
  emit('transfer', { 
    to: transferData.to, 
    amount: transferData.amount 
  });

  transferData.to = '';
  transferData.amount = null;
};
</script>
