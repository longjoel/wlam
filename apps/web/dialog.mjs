export const DIALOG_RESPONSE=0xffffffff;
export function dialogButtons(flags) {
  return [
    [[1,'OK']],[[1,'OK'],[2,'Cancel']],[[3,'Abort'],[4,'Retry'],[5,'Ignore']],
    [[6,'Yes'],[7,'No'],[2,'Cancel']],[[6,'Yes'],[7,'No']],
    [[4,'Retry'],[2,'Cancel']],[[2,'Cancel'],[10,'Try Again'],[11,'Continue']],
  ][flags&15];
}
