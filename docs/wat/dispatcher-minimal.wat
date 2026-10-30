;; Minimal dispatcher: two blocks with a conditional branch between them.
;;
;; This is the reference implementation the Rust emitter must produce. It was
;; written by hand so that the branch structure could be validated and executed
;; independently of any code generator.
;;
;; Guest program (EIP-equivalent flow):
;;   block 0 @ 0x1000: store 1 at 0x100, then branch on (eax < 5)
;;                       if true  -> block 1 @ 0x1010  (store 0xA at 0x104, ret)
;;                       if false -> block 2 @ 0x1014  (store 0xB at 0x104, ret)
;;
;; Block ids: 0 = 0x1000, 1 = 0x1010, 2 = 0x1014 (discovery order).
(module
  (memory (export "memory") 1)

  ;; eax is seeded from the snapshot area at 0x8000 (register slot 0).
  (func (export "entry")
    (local $eax i32)
    (local $id i32)      ;; dispatch id
    (local $cond i32)    ;; CBranch condition

    (local.set $eax (i32.load (i32.const 0x8000)))
    (local.set $id (i32.const 0))

    (loop $dispatch
      ;; The br_table names are listed innermost-first, and each case body sits
      ;; AFTER the `end` of the block whose label it used.
      (block $case0
        (block $case1
          (block $case2
            (br_table $case2 $case1 $case0 $dispatch (local.get $id))
          )
          ;; ---- case 2: 0x1014 ----
          (i32.store (i32.const 0x104) (i32.const 0xB))
          (return)
        )
        ;; ---- case 1: 0x1010 ----
        (i32.store (i32.const 0x104) (i32.const 0xA))
        (return)
      )
      ;; ---- case 0: 0x1000 ----
      (i32.store (i32.const 0x100) (i32.const 1))
      ;; cond = eax < 5  (i.e. `cmp eax,5; jl`)
      (local.set $cond
        (i32.lt_s (local.get $eax) (i32.const 5)))
      (if (local.get $cond)
        (then (local.set $id (i32.const 1)))
        (else (local.set $id (i32.const 2)))
      )
      (br $dispatch)
    )
    unreachable
  )
)
