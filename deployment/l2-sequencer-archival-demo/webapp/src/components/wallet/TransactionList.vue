<template>
  <div class="transaction-list-container w-full mt-6 p-1 border border-gray-100 dark:border-gray-400 rounded-2xl">
    <div> 
      <div class="inline-block min-w-full align-middle">
        <div class="overflow-hidden">
          
          <table class="min-w-full divide-y divide-gray-200 dark:divide-gray-700">
            <thead class="sticky top-0 z-10 ">
              <tr>
                <th scope="col" class="px-5 py-4 text-left text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wider">
                  Status
                </th>
                <th scope="col" class="px-5 py-4 text-left text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wider">
                  Transaction Hash (ID)
                </th>
                <th scope="col" class="px-5 py-4 text-left text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wider">
                  From / To
                </th>
                <th scope="col" class="px-5 py-4 text-right text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wider">
                  Amount
                </th>
              </tr>
            </thead>
            
            <tbody class="divide-y divide-gray-200 dark:divide-gray-700">
              <tr 
                v-for="tx in transactions" 
                :key="tx.id" 
                class="cursor-pointer hover:bg-gray-100 dark:hover:bg-gray-800 transition duration-150"
                @click="emit('selectTransaction', tx)"
              >
                <td class="px-5 py-5 whitespace-nowrap text-sm">
                  <span 
                    :class="[
                      'px-2 py-1 rounded-full text-xs font-medium',
                      tx.confirmed 
                        ? 'bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400' 
                        : 'bg-yellow-100 text-yellow-700 dark:bg-yellow-900/30 dark:text-yellow-400 animate-pulse'
                    ]"
                  >
                    {{ tx.confirmed ? 'Confirmed' : 'Pending' }}
                  </span>
                </td>
                
                <td class="px-5 py-5 text-sm text-gray-700 dark:text-gray-300 font-mono text-xs break-all max-w-[200px]">
                  {{ tx.id }}
                </td>
                
                <td class="px-5 py-5 text-sm text-gray-700 dark:text-gray-300">
                  <div class="flex flex-col">
                    <span class="font-mono text-xs">{{ tx.from }}</span>
                    <div class="text-xs text-gray-500 flex items-center">
                      <span class="mr-1">→</span> {{ tx.to }}
                    </div>
                  </div>
                </td>
                
                <td class="px-5 py-5 whitespace-nowrap text-sm text-right font-semibold">
                  <span :class="tx.from === currentAccount ? 'text-red-600 dark:text-red-400' : 'text-green-600 dark:text-green-400'">
                    {{ tx.from === currentAccount ? '-' : '+' }}{{ tx.amount.toFixed(2) }} MEM
                  </span>
                </td>
              </tr>
              
              <tr v-if="!transactions || transactions.length === 0">
                <td colspan="4" class="px-6 py-4 text-center text-gray-500 dark:text-gray-400">
                  No recent transactions found.
                </td>
              </tr>
            </tbody>
          </table>
          
        </div>
      </div>
    </div>
  </div>
</template>

<script setup>
const props = defineProps({
  transactions: {
    type: Array,
    required: true,
  },
  // We need the current account name to decide if the amount is +/- 
  currentAccount: {
    type: String,
    default: 'Alisa'
  }
});

const emit = defineEmits(['selectTransaction']);
</script>
