import Vue from 'vue';
import { MutationTree } from 'vuex';
import { SwitchStoreState, SynchronizationResult, Switch } from './models';

export default {
    setProcessing(state, processing: boolean) {
        state.processing = processing;
    },
    addError(state, error: Error) {
        state.errors.push(error);
    },
    setSyncResult(state, result: SynchronizationResult) {
        state.syncResult = result;
    },
    setItems(state, items: Switch[]) {
        state.items = items.sort((a, b) => {
            const fqdnA = a.fqdn.toLowerCase();
            const fqdnB = b.fqdn.toLowerCase();
            return (fqdnA < fqdnB) ? -1 : (fqdnA > fqdnB) ? 1 : 0;
        });
    },
    updateItem(state, item: Switch) {
        const index = state.items.findIndex((entry) => entry.fqdn === item.fqdn);
        Vue.set(state.items, index, item);
    }
} as MutationTree<SwitchStoreState>;
