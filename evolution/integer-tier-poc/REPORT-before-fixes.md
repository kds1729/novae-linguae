| record | run | certified | typecheck | effects | refinement | termination | complexity | properties (generative) | prove |
|---|---|---|---|---|---|---|---|---|---|
| `gcd` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=UNVERIFIABLE | UNVERIFIABLE | UNVERIFIABLE | commutative:; right_zero:; divides_both:; associative: | commutative:; right_zero:; divides_both:; associative: |
| `lcm` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=UNVERIFIABLE | UNVERIFIABLE | UNVERIFIABLE | multiple_of_both:; commutative: | multiple_of_both:; commutative: |
| `ext_gcd` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `bezout_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | bezout_identity: | bezout_identity: |
| `mod_inverse` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `inverse_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | inverse_identity: | inverse_identity: |
| `pow_mod_naive` | PASS | True | WELL-TYPED | SOUND | post[0]=UNVERIFIABLE | SOUND | SOUND | — | — |
| `mod_pow` | PASS | True | WELL-TYPED | SOUND | post[0]=UNVERIFIABLE | UNVERIFIABLE | SOUND | — | — |
| `pow_mod_agree` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | fast_equals_naive: | fast_equals_naive: |
| `isqrt` | PASS | True | WELL-TYPED | SOUND | post[0]=UNVERIFIABLE / nat-result (≥ 0)=UNVERIFIABLE | UNVERIFIABLE | SOUND | square_root_of_square:; floor_bounds: | square_root_of_square:; floor_bounds: |
| `mr_squarings` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `mr_witness` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `odd_part` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | SOUND | — | — |
| `mr_all` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `is_prime` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `no_divisor_from` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | SOUND | — | — |
| `is_prime_trial` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `prime_agree` | FAIL | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | miller_rabin_agrees_with_trial: | miller_rabin_agrees_with_trial: |
| `crt2` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `crt_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | solves_both_congruences: | solves_both_congruences: |
| `digits` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `from_digits` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=UNVERIFIABLE | SOUND | SOUND | — | — |
| `digits_roundtrip` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | digits_then_from_digits: | digits_then_from_digits: |
| `digit_sum` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=UNVERIFIABLE | UNVERIFIABLE | UNVERIFIABLE | sum_bounded_by_value: | sum_bounded_by_value: |
| `binomial` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=UNVERIFIABLE | SOUND | SOUND | symmetry:; pascal:; choose_zero: | symmetry:; pascal:; choose_zero: |
| `ntt_split` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `ntt_butterfly` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `ntt_rec` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `ntt` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `ntt_inverse` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `ntt_roundtrip` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `pointwise_mod` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | length_is_min: | length_is_min: |
| `pad_to` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | UNVERIFIABLE | length_is_n: | length_is_n: |
| `next_pow2` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=UNVERIFIABLE | UNVERIFIABLE | UNVERIFIABLE | at_least_n:; under_twice_n: | at_least_n:; under_twice_n: |
| `convolution` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
