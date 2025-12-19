import { defineStore } from 'pinia';

const BASE_URL = import.meta.env.VITE_ARCHIVER_URL || 'http://localhost:8090';
const EXPLORER_URL = import.meta.env.VITE_EXPLORER_URL || 'http://localhost:8000';
const STREAM_URL = `${BASE_URL}/block_stream`;
const CACHE_URL = `${BASE_URL}/blocks`;

export const useArchiveStore = defineStore('archive', {
  state: () => ({
    blocks: [],
    loading: false,
    eventSource: null,
    // Status can be: 'disconnected', 'waiting', 'connected', or 'error'
    connectionStatus: 'disconnected',
  }),

  actions: {
    processBlocks(blocks) {
      return blocks.map(block => ({
        ...block,
        data: {
          ...block.data,
          transactions: (block.data?.transactions || []).map(tx => ({
            ...tx,
            confirmed: true // Overwriting the pending status from the raw data
          }))
        }
      }));
    },

    async fetchCachedBlocks() {
      this.loading = true;
      try {
        const res = await fetch(CACHE_URL);
        if (!res.ok) throw new Error('Failed to fetch cached blocks');

        const data = await res.json();
        const processed = this.processBlocks(data);

        // Reverse so newest blocks (highest ID) are at the top
        this.blocks = [...processed].reverse().slice(0, 50);
      } catch (err) {
        console.error("Error fetching cached blocks:", err);
      } finally {
        this.loading = false;
      }
    },

    startStream() {
      if (this.eventSource) return;
      this.connectionStatus = 'connecting';
      this.eventSource = new EventSource(STREAM_URL);

      this.eventSource.onopen = () => {
        this.connectionStatus = 'connected';
        console.log("Archive Stream Connected");
      };

      this.eventSource.onmessage = (event) => {
        try {
          const rawData = JSON.parse(event.data);
          const rawBlocks = Array.isArray(rawData) ? rawData : [rawData];

          const processed = this.processBlocks(rawBlocks);

          // Prepend new blocks to the beginning of the state
          this.blocks = [...processed.reverse(), ...this.blocks].slice(0, 50);
        } catch (err) {
          console.error("Error parsing stream data:", err);
        }
      };

      this.eventSource.onerror = () => {
        this.connectionStatus = 'error';
        this.stopStream();
        setTimeout(() => this.startStream(), 5000);
      };
    },

    stopStream() {
      if (this.eventSource) {
        this.eventSource.close();
        this.eventSource = null;
        this.connectionStatus = 'disconnected';
      }
    },

    openBlockInExplorer(blockId) {
      if (!blockId) return;
      window.open(`${EXPLORER_URL}/blocks/${blockId}`, '_blank');
    },

    openTransactionInExplorer(txId) {
      if (!txId) return;
      window.open(`${EXPLORER_URL}/transactions/${txId}`, '_blank');
    },
  }
});