import { defineStore } from 'pinia';

const BASE_URL = import.meta.env.VITE_API_BASE_URL || 'http://localhost:8080';
const STREAM_URL = `${BASE_URL}/block_stream`;

export const useArchiveStore = defineStore('archive', {
  state: () => ({
    transactions: [],
    loading: false,
    eventSource: null,
    // Status can be: 'disconnected', 'connecting', 'connected', or 'error'
    connectionStatus: 'disconnected', 
  }),

  actions: {
    startStream() {
      if (this.eventSource) return;

      this.loading = true;
      this.connectionStatus = 'connecting';
      
      this.eventSource = new EventSource(STREAM_URL);

      this.eventSource.onopen = () => {
        this.loading = false;
        this.connectionStatus = 'connected';
        console.log("Archive Stream Connected");
      };

      this.eventSource.onmessage = (event) => {
        try {
          const blockData = JSON.parse(event.data);
          
          if (blockData?.transactions?.length) {
            const newTxs = blockData.transactions.map(tx => ({
              ...tx,
              confirmed: true
            }));

            this.transactions = [...newTxs, ...this.transactions].slice(0, 100);
          }
        } catch (err) {
          console.error("Error parsing block data:", err);
        }
      };

      this.eventSource.onerror = (err) => {
        console.error("Archive Stream Error:", err);
        this.connectionStatus = 'error';
        this.stopStream();
        
        // Attempt reconnection after 5 seconds
        setTimeout(() => this.startStream(), 5000);
      };
    },

    stopStream() {
      if (this.eventSource) {
        this.eventSource.close();
        this.eventSource = null;
        this.loading = false;
        this.connectionStatus = 'disconnected';
        console.log("Archive Stream Disconnected");
      }
    }
  }
});
