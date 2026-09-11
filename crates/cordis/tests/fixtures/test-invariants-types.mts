import {
  TEST_INVARIANT_READY_SERVICE,
  createTestInvariantAttachmentStore,
  installTestInvariantHost,
  testInvariantCompanionPaths,
  usesManualInvariantTree,
} from '@seekdeep-ai/cordis';

const ready: 'testInvariantReady' = TEST_INVARIANT_READY_SERVICE;
const manual: boolean = usesManualInvariantTree('/repo/packages/core/tools/tests/invariant.spec.ts');
const companions: string[] = testInvariantCompanionPaths('/repo/scripts/test-invariants.spec.ts', {});
const attachment: Function = createTestInvariantAttachmentStore(class AttachmentStore {});
const dispose: Function = installTestInvariantHost({}, () => {}, attachment, Error, () => '', {});
void [ready, manual, companions, dispose];
