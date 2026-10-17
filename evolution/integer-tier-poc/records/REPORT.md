| record | run | certified | typecheck | effects | refinement | termination | complexity | properties (generative) | prove |
|---|---|---|---|---|---|---|---|---|---|
| `gcd` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=UNVERIFIABLE | SOUND | SOUND | commutative=EXHAUSTIVE; right_zero=EXHAUSTIVE; divides_both=EXHAUSTIVE; associative=EXHAUSTIVE | commutative=UNSUPPORTED; right_zero=UNSUPPORTED; divides_both=UNSUPPORTED; associative=UNSUPPORTED |
| `lcm` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=UNVERIFIABLE | UNVERIFIABLE | UNVERIFIABLE | multiple_of_both=EXHAUSTIVE; commutative=EXHAUSTIVE | multiple_of_both=UNSUPPORTED; commutative=UNSUPPORTED |
| `ext_gcd` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `bezout_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | bezout_identity=EXHAUSTIVE | bezout_identity=UNSUPPORTED |
| `mod_inverse` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `inverse_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | inverse_identity=EXHAUSTIVE | inverse_identity=UNSUPPORTED |
| `mod_add` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND | SOUND | SOUND | congruent_to_plain_sum=EXHAUSTIVE; commutative=EXHAUSTIVE | congruent_to_plain_sum=UNKNOWN; commutative=PROVED |
| `mod_mul` | PASS | True | WELL-TYPED | SOUND | post[0]=SOUND | SOUND | SOUND | congruent_to_plain_product=EXHAUSTIVE; commutative=EXHAUSTIVE | congruent_to_plain_product=UNKNOWN; commutative=PROVED |
| `is_even` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | period_two=EXHAUSTIVE; alternates=EXHAUSTIVE | period_two=PROVED; alternates=PROVED |
| `pow_mod_naive` | PASS | True | WELL-TYPED | SOUND | post[0]=UNVERIFIABLE | SOUND | SOUND | — | — |
| `mod_pow` | PASS | True | WELL-TYPED | SOUND | post[0]=UNVERIFIABLE | SOUND | SOUND | — | — |
| `pow_mod_agree` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | fast_equals_naive=EXHAUSTIVE | fast_equals_naive=UNSUPPORTED |
| `isqrt` | PASS | True | WELL-TYPED | SOUND | post[0]=UNVERIFIABLE / nat-result (≥ 0)=UNVERIFIABLE | SOUND | SOUND | square_root_of_square=EXHAUSTIVE; floor_bounds=EXHAUSTIVE | square_root_of_square=UNSUPPORTED; floor_bounds=UNSUPPORTED |
| `mr_squarings` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `mr_witness` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `odd_part` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `mr_all` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `is_prime` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `no_divisor_from` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `is_prime_trial` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `prime_agree` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | miller_rabin_agrees_with_trial=EXHAUSTIVE | miller_rabin_agrees_with_trial=UNSUPPORTED |
| `crt2` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `crt_holds` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | solves_both_congruences=HELD | solves_both_congruences=UNSUPPORTED |
| `digits` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `from_digits` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=UNVERIFIABLE | SOUND | SOUND | — | — |
| `digits_roundtrip` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | digits_then_from_digits=EXHAUSTIVE | digits_then_from_digits=UNSUPPORTED |
| `digit_sum` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=UNVERIFIABLE | UNVERIFIABLE | UNVERIFIABLE | sum_bounded_by_value=EXHAUSTIVE | sum_bounded_by_value=UNSUPPORTED |
| `binomial` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=UNVERIFIABLE | SOUND | SOUND | symmetry=EXHAUSTIVE; pascal=EXHAUSTIVE; choose_zero=EXHAUSTIVE | symmetry=UNSUPPORTED; pascal=UNSUPPORTED; choose_zero=UNSUPPORTED |
| `ntt_split` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `ntt_butterfly` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | — | — |
| `ntt_rec` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `ntt` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `ntt_inverse` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `ntt_roundtrip` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
| `pointwise_mod` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | SOUND | length_is_min=HELD | length_is_min=UNSUPPORTED |
| `pad_to` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | SOUND | UNVERIFIABLE | length_is_n=EXHAUSTIVE | length_is_n=UNSUPPORTED |
| `next_pow2` | PASS | True | WELL-TYPED | SOUND | nat-result (≥ 0)=UNVERIFIABLE | SOUND | SOUND | at_least_n=EXHAUSTIVE; under_twice_n=EXHAUSTIVE | at_least_n=UNSUPPORTED; under_twice_n=UNSUPPORTED |
| `convolution` | PASS | True | WELL-TYPED | SOUND | refinements=N/A | UNVERIFIABLE | UNVERIFIABLE | — | — |
