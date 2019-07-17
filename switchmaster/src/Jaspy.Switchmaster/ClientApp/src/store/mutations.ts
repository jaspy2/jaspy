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
        state.items = items;
    },
} as MutationTree<SwitchStoreState>;
